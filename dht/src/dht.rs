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

#[derive(Debug)]
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
        let node_id = NodeId::from_bytes([0u8; 20]);

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

        if socket_v4.is_none() && socket_v6.is_none() {
            panic!("Failed to bind any DHT sockets");
        }
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

    fn send_message(&self, addr: SocketAddr, message_payload: &[u8]) {
        let socket = if addr.is_ipv4() {
            self.socket_v4.clone()
        } else {
            self.socket_v6.clone()
        };

        if let Some(s) = socket {
            _ = s.try_send_to(message_payload, addr);
        }
    }

    const fn next_tid(&mut self) -> u16 {
        let id = self.next_tid;
        self.next_tid = self.next_tid.wrapping_add(1);
        id
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

        self.send_message(addr, &ping_message.to_bytes());
    }

    async fn bootstrap(&mut self, bootstrap_nodes: Vec<BootstrapNode>) {
        let mut addr_resolution_tasks = bootstrap_nodes
            .into_iter()
            .map(|node| async move {
                let addr_str = format!("{}:{}", node.host, node.port);
                lookup_host(addr_str).await
            })
            .collect::<FuturesUnordered<_>>();

        while let Some(result) = addr_resolution_tasks.next().await {
            let Ok(mut addrs) = result else { continue };
            let Some(addr) = addrs.next() else { continue };
            self.ping_node(addr);
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
            }
        }
    }

    async fn handle_command(&mut self, cmd: DhtCommand) {}

    async fn handle_incoming(&mut self, (addr, bytes): (SocketAddr, Bytes)) {
        let Ok(msg) = KrpcMessage::from_bytes(&bytes) else {
            return;
        };

        match msg.body {
            MessageBody::Query(query) => {
                self.handle_query(addr, msg.transaction_id, query).await;
            }
            MessageBody::Response(response) => {
                let key = InflightKey { addr, t: 0 };
                let Some(entry) = self.inflight.remove(&key) else {
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
            (QueryKind::Ping, Response::Ping { id }) => {
                self.try_add_node(addr, id);
            }
            (QueryKind::FindNode, Response::FindNode { id, nodes }) => {
                self.try_add_node(addr, id);
                for node in nodes {
                    // queue further pings / routing table population
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
                // handle peers / closer nodes
            }
            _ => {
                // response kind does not match what we sent — protocol error, ignore
                tracing::debug!("mismatched response kind from {addr}");
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

    fn try_add_node(&mut self, addr: SocketAddr, node_id: NodeId) {
        match addr {
            SocketAddr::V4(v4) => {
                if let Some(InsertResult::NeedsPing {
                    stale_addr,
                    stale_id,
                    ..
                }) = self.routing_table.try_insert_v4(node_id, v4)
                {
                    self.ping_node(stale_addr.into());
                }
            }
            SocketAddr::V6(v6) => {
                if let Some(InsertResult::NeedsPing {
                    stale_addr,
                    stale_id,
                    ..
                }) = self.routing_table.try_insert_v6(node_id, v6)
                {
                    self.ping_node(stale_addr.into());
                }
            }
        }
    }
}
