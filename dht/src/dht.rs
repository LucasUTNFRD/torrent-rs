use crate::{
    error::DhtError,
    message::{KrpcMessage, MessageBody, Query, Response, TransactionId},
    routing_table::{InsertResult, NodeId, RoutingTable},
};
use bittorrent_common::types::InfoHash;
use bytes::{Bytes, BytesMut};
use futures::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{UdpSocket, lookup_host},
    sync::{mpsc, oneshot},
};

const DEFAULT_PORT: u16 = 6881;

const MAX_PAYLOAD: usize = 2048;

#[derive(Debug, Clone)]
pub struct DhtConfig {
    pub port: u16,
    /// Bootstrap nodes used when the routing table is empty.
    /// Defaults to [`default_dht_bootstrap_nodes()`] if not set.
    pub bootstrap_nodes: Vec<BootstrapNode>,
}

impl DhtConfig {
    pub fn builder() -> DhtConfigBuilder {
        DhtConfigBuilder::default()
    }
}

impl Default for DhtConfig {
    fn default() -> Self {
        DhtConfigBuilder::default().build()
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapNode {
    pub host: String,
    pub port: u16,
}

impl BootstrapNode {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

/// Well-known public bootstrap nodes shipped as a convenience default.
pub fn default_dht_bootstrap_nodes() -> Vec<BootstrapNode> {
    vec![
        BootstrapNode::new("router.bittorrent.com", 6881),
        BootstrapNode::new("dht.transmissionbt.com", 6881),
        BootstrapNode::new("dht.libtorrent.org", 25401),
        BootstrapNode::new("router.utorrent.com", 6881),
    ]
}

// ---------------------------------------------------------------------------
// DhtConfigBuilder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DhtConfigBuilder {
    port: u16,
    bootstrap_nodes: Option<Vec<BootstrapNode>>,
}

impl Default for DhtConfigBuilder {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            bootstrap_nodes: None,
        }
    }
}

impl DhtConfigBuilder {
    pub const fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Replace the bootstrap list entirely.
    pub fn bootstrap_nodes(mut self, nodes: Vec<BootstrapNode>) -> Self {
        self.bootstrap_nodes = Some(nodes);
        self
    }

    /// Append a single node to the list (initialises from defaults if not yet set).
    pub fn bootstrap_node(mut self, host: impl Into<String>, port: u16) -> Self {
        self.bootstrap_nodes
            .get_or_insert_with(default_dht_bootstrap_nodes)
            .push(BootstrapNode::new(host, port));
        self
    }

    pub fn build(self) -> DhtConfig {
        DhtConfig {
            port: self.port,
            bootstrap_nodes: self
                .bootstrap_nodes
                .unwrap_or_else(default_dht_bootstrap_nodes),
        }
    }
}

// Public API
pub struct Dht {
    tx: mpsc::Sender<DhtCommand>,
}

#[derive(Debug, Clone, Copy)]
pub enum SwarmStatus {
    Broken,     // Too few nodes
    Poor,       // Some nodes but not many
    Firewalled, // Has nodes but not receiving incoming
    Good,       // Fully functional
}

enum DhtCommand {
    Announce {
        info_hash: InfoHash,
        peer_port: u16,
        tx: oneshot::Sender<Vec<SocketAddr>>,
    },
    FindPeer {
        info_hash: InfoHash,
        tx: oneshot::Sender<Vec<SocketAddr>>,
    },
    AddNode {
        addr: SocketAddr,
    },
    SwarmStatus {
        tx: oneshot::Sender<SwarmStatus>,
    },
}

impl Dht {
    pub async fn new(cfg: DhtConfig) -> Result<Self, DhtError> {
        let (tx, rx) = mpsc::channel(128);
        let dht_inner = DhtActor::new(&cfg, rx).await?;

        tokio::spawn(async move { dht_inner.run(cfg.bootstrap_nodes).await });

        Ok(Self { tx })
    }

    pub async fn announce(
        &self,
        info_hash: InfoHash,
        peer_port: u16,
    ) -> Result<Vec<SocketAddr>, DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::Announce {
                info_hash,
                peer_port,
                tx,
            })
            .await
            .map_err(|_| DhtError::Cancelled)?;
        rx.await.map_err(|_| DhtError::Cancelled)
    }

    pub async fn find_peers(&self, info_hash: InfoHash) -> Result<Vec<SocketAddr>, DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::FindPeer { info_hash, tx })
            .await
            .map_err(|_| DhtError::Cancelled)?;
        rx.await.map_err(|_| DhtError::Cancelled)
    }

    pub async fn get_swarm_status(&self) -> Result<SwarmStatus, DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::SwarmStatus { tx })
            .await
            .map_err(|_| DhtError::Cancelled)?;
        rx.await.map_err(|_| DhtError::Cancelled)
    }
}

struct DhtActor {
    rx: mpsc::Receiver<DhtCommand>,
    node_id: NodeId,

    // The two DHTs are independent, meaning that no IPv6 data is stored in the IPv4 DHT, and, conversely, no IPv4 data is stored in the IPv6 DHT. A node wishing to participate in both DHTs must maintain two distinct routing tables, one for IPv4 and one for IPv6.
    socket_v4: Option<Arc<UdpSocket>>,
    socket_v6: Option<Arc<UdpSocket>>,
    routing_table: RoutingTable,

    inflight: HashMap<InflightKey, InflightEntry>,
    next_tid: u16,
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct InflightKey {
    addr: SocketAddr,
    t: u16,
}

struct InflightEntry {
    query_kind: QueryKind,
}

#[derive(Debug, Clone, Copy)]
enum QueryKind {
    Ping,
    FindNode,
    GetPeers,
    AnnouncePeer,
}

async fn run_socket(
    socket: Arc<UdpSocket>,
    tx: mpsc::Sender<(SocketAddr, Bytes)>,
) -> Result<(), DhtError> {
    let mut buf = BytesMut::with_capacity(MAX_PAYLOAD);

    loop {
        // recv_buf_from appends the incoming data and updates the `buf` length.
        let (_len, addr) = socket.recv_buf_from(&mut buf).await?;

        // .split() extracts the read bytes into a new BytesMut, leaving the original empty.
        // .freeze() converts it to an immutable Bytes, which is cheap to clone and share.
        let bytes = buf.split().freeze();

        if tx.send((addr, bytes)).await.is_err() {
            break;
        }

        // Ensure the buffer has enough contiguous capacity for the next read.
        // If the previously frozen `Bytes` have been dropped, BytesMut reuses the allocation.
        buf.reserve(MAX_PAYLOAD);
    }

    Ok(())
}

impl DhtActor {
    fn get_nodes_from_bootstrap_file() {}

    pub async fn new(cfg: &DhtConfig, rx: mpsc::Receiver<DhtCommand>) -> Result<Self, DhtError> {
        // TODO: Persist DHT Node Id
        let node_id = NodeId::from_bytes(rand::random::<[u8; 20]>());

        // Bind IPv4 socket
        let socket_v4 = UdpSocket::bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, cfg.port))
            .await
            .map(Arc::new)
            .ok();

        // Bind IPv6 socket with IPV6_V6ONLY flag enforced
        let socket_v6 = {
            let socket = socket2::Socket::new(
                socket2::Domain::IPV6,
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            )?;
            socket.set_only_v6(true)?;
            socket.set_nonblocking(true)?;
            socket.bind(&SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), cfg.port).into())?;
            UdpSocket::from_std(socket.into()).map(Arc::new)
        }
        .ok();

        assert!(
            !(socket_v4.is_none() && socket_v6.is_none()),
            "Failed to bind any DHT sockets"
        );

        let routing_table = RoutingTable::new(node_id);

        Ok(Self {
            rx,
            node_id,
            socket_v4,
            socket_v6,
            next_tid: 0,
            inflight: HashMap::default(),
            routing_table,
        })
    }

    const fn next_tid(&mut self) -> u16 {
        let id = self.next_tid;
        self.next_tid = self.next_tid.wrapping_add(1);
        id
    }

    fn send_message(&self, addr: SocketAddr, message_payload: &[u8]) {
        let socket = if addr.is_ipv4() {
            self.socket_v4.clone()
        } else {
            self.socket_v6.clone()
        };

        if let Some(s) = socket {
            if let Err(e) = s.try_send_to(message_payload, addr) {
                tracing::warn!("Failed to send to {}: {}", addr, e);
            }
        } else {
            tracing::warn!("No socket available for {}", addr);
        }
    }

    fn ping_node(&mut self, addr: SocketAddr) {
        let tx_id = self.next_tid();
        let ping_message = KrpcMessage::ping_query(tx_id, self.node_id);

        self.inflight.insert(
            InflightKey { addr, t: tx_id },
            InflightEntry {
                query_kind: QueryKind::Ping,
            },
        );

        tracing::debug!("Sending ping to {} (tid={})", addr, tx_id);
        self.send_message(addr, &ping_message.to_bytes());
    }

    fn find_node(&mut self, addr: SocketAddr, target: NodeId) {
        let tx_id = self.next_tid();
        let msg =
            KrpcMessage::find_node_query(tx_id, self.node_id, target /*TODO: num want arg*/);

        self.inflight.insert(
            InflightKey { addr, t: tx_id },
            InflightEntry {
                query_kind: QueryKind::FindNode,
            },
        );

        self.send_message(addr, &msg.to_bytes());
    }

    fn send_get_peers(&mut self, addr: SocketAddr, info_hash: InfoHash) {
        let tx_id = self.next_tid();
        let msg = KrpcMessage::get_peers_query(tx_id, self.node_id, info_hash);

        self.inflight.insert(
            InflightKey { addr, t: tx_id },
            InflightEntry {
                query_kind: QueryKind::GetPeers,
            },
        );

        tracing::debug!(
            "Sending get_peers to {} for info_hash {} (tid={})",
            addr,
            hex::encode(info_hash.as_slice()),
            tx_id
        );
        self.send_message(addr, &msg.to_bytes());
    }

    async fn bootstrap(&mut self, bootstrap_nodes: Vec<BootstrapNode>) {
        tracing::info!(
            "Starting DHT bootstrap with {} nodes",
            bootstrap_nodes.len()
        );

        let mut addr_resolution_tasks = bootstrap_nodes
            .into_iter()
            .map(|node| async move {
                let addr_str = format!("{}:{}", node.host, node.port);
                lookup_host(addr_str).await.map(|a| (node.host, a))
            })
            .collect::<FuturesUnordered<_>>();

        while let Some(result) = addr_resolution_tasks.next().await {
            let Ok((host, mut addrs)) = result else {
                continue;
            };
            let Some(addr) = addrs.next() else { continue };
            tracing::debug!("Resolved bootstrap node {} -> {}", host, addr);

            let info_hash =
                InfoHash::from_slice(self.node_id.as_slice()).expect("it is our own node id");
            self.send_get_peers(addr, info_hash);
        }
    }

    pub async fn run(mut self, bootstrap_nodes: Vec<BootstrapNode>) {
        self.bootstrap(bootstrap_nodes).await;

        let (packet_tx, mut packet_rx) = mpsc::channel::<(SocketAddr, Bytes)>(128);

        if let Some(socket_v4) = &self.socket_v4 {
            let socket = socket_v4.clone();
            let tx = packet_tx.clone();
            tokio::spawn(async move {
                let _ = run_socket(socket, tx).await;
            });
        }

        if let Some(socket_v6) = &self.socket_v6 {
            let socket = socket_v6.clone();
            let tx = packet_tx.clone();
            tokio::spawn(async move {
                let _ = run_socket(socket, tx).await;
            });
        }
        let mut maintenance_interval = tokio::time::interval(Duration::from_secs(60));
        let mut search_interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await,
                        None => break, // command sender dropped
                    }
                }
                packet = packet_rx.recv() => {
                    match packet {
                        Some(packet) => {
                            self.handle_incoming(packet).await;
                        }
                        None => break, // All socket tasks terminated
                    }
                }
               // Bucket/maintenance timer
                _ = maintenance_interval.tick() => {

               }
               // Search step timer
               _ = search_interval.tick() => {
               }
            }
        }
    }

    async fn maintenance_interval(&mut self) {}

    async fn search_interval(&mut self) {}

    async fn handle_command(&mut self, cmd: DhtCommand) {
        match cmd {
            DhtCommand::SwarmStatus { tx } => {
                let status = self.compute_swarm_status();
                let _ = tx.send(status);
            }
            DhtCommand::AddNode { addr } => {}
            DhtCommand::Announce {
                info_hash,
                peer_port,
                tx,
            } => {}
            DhtCommand::FindPeer { info_hash, tx } => {}
        }
    }

    fn compute_swarm_status(&self) -> SwarmStatus {
        let good_count = self.routing_table.good_nodes_count();

        if good_count >= 10 {
            SwarmStatus::Good
        } else if good_count >= 3 {
            SwarmStatus::Poor
        } else {
            SwarmStatus::Broken
        }
    }

    async fn handle_incoming(&mut self, (addr, bytes): (SocketAddr, Bytes)) {
        let Ok(msg) = KrpcMessage::from_bytes(&bytes) else {
            return;
        };

        match msg.body {
            MessageBody::Query(query) => {
                tracing::debug!("Received query from {}: {:?}", addr, query);
                self.handle_query(addr, msg.transaction_id, query).await;
            }
            MessageBody::Response(response) => {
                let tid = u16::from_be_bytes([msg.transaction_id.0[0], msg.transaction_id.0[1]]);
                tracing::debug!(
                    "Received {:?} response from {} (tid={})",
                    response,
                    addr,
                    tid
                );
                let key = InflightKey { addr, t: tid };
                let Some(entry) = self.inflight.remove(&key) else {
                    tracing::warn!(
                        "Received response from {} but no matching inflight request",
                        addr
                    );
                    return;
                };
                self.handle_response(addr, entry, response);
            }
            MessageBody::Error { code, message } => {
                tracing::error!(code, message);
            }
        }
    }

    fn handle_response(&mut self, addr: SocketAddr, entry: InflightEntry, response: Response) {
        match (entry.query_kind, response) {
            (QueryKind::Ping, Response::Ping { id })
            | (QueryKind::GetPeers, Response::Ping { id }) => {
                tracing::info!(
                    "Ping-like response from {} - node_id={}",
                    addr,
                    hex::encode(id.as_slice())
                );
                let _ = self.try_add_node(addr, id);

                if matches!(entry.query_kind, QueryKind::Ping) {
                    let info_hash = InfoHash::from_slice(self.node_id.as_slice())
                        .expect("it is our own node id");
                    self.send_get_peers(addr, info_hash);
                }
            }
            (QueryKind::FindNode, Response::FindNode { id, nodes })
            | (QueryKind::GetPeers, Response::FindNode { id, nodes }) => {
                tracing::info!(
                    "FindNode-like response from {} - node_id={}",
                    addr,
                    hex::encode(id.as_slice())
                );
                self.try_add_node(addr, id);
                tracing::debug!("Found {} nodes in response", nodes.len());
                for node in nodes {
                    tracing::debug!(
                        "  - Adding discovered node {} at {}",
                        hex::encode(node.node_id.as_slice()),
                        node.addr
                    );
                    self.try_add_node(node.addr, node.node_id);
                }
            }
            (
                QueryKind::GetPeers,
                Response::GetPeers {
                    id,
                    token,
                    values,
                    nodes,
                },
            ) => {
                tracing::info!(
                    "GetPeers response from {} - node_id={}, token={}",
                    addr,
                    hex::encode(id.as_slice()),
                    hex::encode(&token)
                );
                self.try_add_node(addr, id);

                if let Some(nodes) = nodes {
                    tracing::debug!("Found {} closer nodes in GetPeers response", nodes.len());
                    for node in nodes {
                        tracing::debug!(
                            "  - Adding discovered node {} at {}",
                            hex::encode(node.node_id.as_slice()),
                            node.addr
                        );
                        self.try_add_node(node.addr, node.node_id);
                    }
                }

                if let Some(peers) = values {
                    tracing::debug!("Found {} peer values in GetPeers response", peers.len());
                    for peer in peers {
                        tracing::debug!("  - Peer value: {}", peer);
                    }
                }
            }
            _ => {
                tracing::debug!("Mismatched response kind from {addr}");
            }
        }
    }

    async fn handle_query(
        &mut self,
        addr: SocketAddr,
        transaction_id: TransactionId,
        query: Query,
    ) {
        todo!()
    }

    fn try_add_node(&mut self, addr: SocketAddr, node_id: NodeId) -> Option<InsertResult> {
        match addr {
            SocketAddr::V4(v4) => self.routing_table.try_insert_v4(node_id, v4),
            SocketAddr::V6(v6) => self.routing_table.try_insert_v6(node_id, v6),
        }
    }
}
