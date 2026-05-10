use crate::{
    error::DhtError,
    message::{KrpcMessage, MessageBody, Query, Response, TransactionId},
    node_id::NodeId,
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

const ALPHA: usize = 5;
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

#[derive(Debug)]
pub(crate) enum DhtCommand {
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
        id: NodeId,
        addr: SocketAddr,
    },
    SwarmStatus {
        tx: oneshot::Sender<SwarmStatus>,
    },

    /// Send an outgoing KRPC Query (Called by LookupTasks)
    SendQuery {
        addr: SocketAddr,
        query: Query,
        callback: oneshot::Sender<Response>,
    },
}

impl Dht {
    pub async fn new(cfg: DhtConfig) -> Result<Self, DhtError> {
        let (tx, dht_inner) = DhtActor::new(&cfg).await?;

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
    tx: mpsc::Sender<DhtCommand>,
    node_id: NodeId,

    socket_v4: Option<Arc<UdpSocket>>,
    socket_v6: Option<Arc<UdpSocket>>,

    // routing_table_v4: Arc<RwLock<KTable<SocketAddrV4>>>,
    // routing_table_v6: Arc<RwLock<KTable<SocketAddrV6>>>,
    pending_queries: HashMap<u16, oneshot::Sender<Response>>,
    next_tid: u16,
}

async fn run_socket(
    socket: Arc<UdpSocket>,
    tx: mpsc::Sender<(SocketAddr, Bytes)>,
) -> Result<(), DhtError> {
    let mut buf = BytesMut::with_capacity(MAX_PAYLOAD);

    loop {
        let (_len, addr) = socket.recv_buf_from(&mut buf).await?;
        let bytes = buf.split().freeze();

        if tx.send((addr, bytes)).await.is_err() {
            break;
        }

        buf.reserve(MAX_PAYLOAD);
    }

    Ok(())
}

impl DhtActor {
    pub async fn new(cfg: &DhtConfig) -> Result<(mpsc::Sender<DhtCommand>, Self), DhtError> {
        let (tx, rx) = mpsc::channel(128);
        let node_id = NodeId::from_bytes(rand::random::<[u8; 20]>());

        let socket_v4 = UdpSocket::bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, cfg.port))
            .await
            .map(Arc::new)
            .ok();

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

        // let routing_table_v4 = Arc::new(RwLock::new(KTable::new(node_id)));
        // let routing_table_v6 = Arc::new(RwLock::new(KTable::new(node_id)));

        Ok((
            tx.clone(),
            Self {
                rx,
                tx,
                node_id,
                socket_v4,
                socket_v6,
                next_tid: 0,
                pending_queries: HashMap::default(),
                // routing_table_v4,
                // routing_table_v6,
            },
        ))
    }

    const fn next_tid(&mut self) -> u16 {
        let id = self.next_tid;
        self.next_tid = self.next_tid.wrapping_add(1);
        id
    }

    async fn bootstrap(&mut self, bootstrap_nodes: Vec<BootstrapNode>) {
        let bootstrap_addrs: Vec<SocketAddr> = bootstrap_nodes
            .iter()
            .map(|node| {
                let addr = format!("{}:{}", node.host, node.port);
                async move { lookup_host(addr).await }
            })
            .collect::<FuturesUnordered<_>>()
            .filter_map(|result| async move { result.ok() })
            .flat_map(futures::stream::iter)
            .collect()
            .await;

        let dht_tx = self.tx.clone();
        let our_node_id = self.node_id;

        // if self.socket_v4.is_some() {
        //     let v4_bootstrap: Vec<SocketAddrV4> = bootstrap_addrs
        //         .iter()
        //         .filter_map(|a| {
        //             if let SocketAddr::V4(v4) = a {
        //                 Some(*v4)
        //             } else {
        //                 None
        //             }
        //         })
        //         .collect();
        //
        //     tokio::spawn(bootstrap_task(
        //         our_node_id,
        //         self.routing_table_v4.clone(),
        //         dht_tx.clone(),
        //         v4_bootstrap,
        //     ));
        // }
        //
        // if self.socket_v6.is_some() {
        //     let v6_bootstrap: Vec<SocketAddrV6> = bootstrap_addrs
        //         .iter()
        //         .filter_map(|a| {
        //             if let SocketAddr::V6(v6) = a {
        //                 Some(*v6)
        //             } else {
        //                 None
        //             }
        //         })
        //         .collect();
        //
        //     tokio::spawn(bootstrap_task(
        //         our_node_id,
        //         self.routing_table_v6.clone(),
        //         dht_tx,
        //         v6_bootstrap,
        //     ));
        // }
    }

    fn start_socket_task(&self) -> mpsc::Receiver<(SocketAddr, Bytes)> {
        let (packet_tx, packet_rx) = mpsc::channel::<(SocketAddr, Bytes)>(128);

        if let Some(socket_v4) = &self.socket_v4 {
            let socket = socket_v4.clone();
            let tx = packet_tx.clone();
            tokio::spawn(async move {
                let _ = run_socket(socket, tx).await;
            });
        }

        if let Some(socket_v6) = &self.socket_v6 {
            let socket = socket_v6.clone();
            let tx = packet_tx;
            tokio::spawn(async move {
                let _ = run_socket(socket, tx).await;
            });
        }

        packet_rx
    }

    pub async fn run(mut self, bootstrap_nodes: Vec<BootstrapNode>) {
        let mut packet_rx = self.start_socket_task();

        self.bootstrap(bootstrap_nodes).await;

        let mut maintenance_interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await,
                        None => break,
                    }
                }
                packet = packet_rx.recv() => {
                    match packet {
                        Some(packet) => {
                            self.handle_incoming(packet);
                        }
                        None => break,
                    }
                }
                _ = maintenance_interval.tick() => {
                    // TODO: Maintenance
               }
            }
        }
    }

    async fn handle_command(&mut self, cmd: DhtCommand) {
        match cmd {
            DhtCommand::SwarmStatus { tx } => {
                let status = self.compute_swarm_status();
                let _ = tx.send(status);
            }
            DhtCommand::AddNode { id, addr } => {}
            DhtCommand::Announce { .. } => {}
            DhtCommand::FindPeer { .. } => {}
            DhtCommand::SendQuery {
                addr,
                query,
                callback,
            } => {
                let tid = self.next_tid();
                let _ = self.pending_queries.insert(tid, callback);

                tracing::debug!("Sending KRPC query to {}: {:?}", addr, query);

                let packet = KrpcMessage {
                    transaction_id: TransactionId::new(tid),
                    version: None,
                    sender_ip: None,
                    body: MessageBody::Query(query),
                }
                .to_bytes();

                let socket = if addr.is_ipv4() {
                    self.socket_v4.clone()
                } else {
                    self.socket_v6.clone()
                };

                if let Some(socket) = socket {
                    let s = socket;
                    tokio::spawn(async move {
                        let _ = s.send_to(&packet, addr).await;
                    });
                }
            }
        }
    }

    fn compute_swarm_status(&self) -> SwarmStatus {
        todo!()
    }

    fn handle_incoming(&mut self, (addr, bytes): (SocketAddr, Bytes)) {
        let Ok(msg) = KrpcMessage::from_bytes(&bytes) else {
            return;
        };

        tracing::debug!("Received KRPC message from {}: {:?}", addr, msg);

        if let Some(callback) = self.pending_queries.remove(&msg.transaction_id.as_u16()) {
            if let MessageBody::Response(res) = msg.body {
                let _ = callback.send(res);
            }
            return;
        }

        if let MessageBody::Query(query) = msg.body {
            match query {
                Query::Ping { id } => {}
                Query::FindNode { id, target, .. } => {}
                Query::GetPeers {
                    id,
                    info_hash,
                    is_bootstrap,
                } => {}
                Query::AnnouncePeer {
                    id,
                    info_hash,
                    port,
                    token,
                    implied_port,
                } => {}
            }
        }

        // if let Some(id) = msg.get_node_id() {
        //     if let SocketAddr::V4(v4_addr) = addr {
        //         self.routing_table_v4
        //             .write()
        //             .expect("lock poisoned")
        //             .try_insert(id, v4_addr);
        //     } else if let SocketAddr::V6(v6_addr) = addr {
        //         self.routing_table_v6
        //             .write()
        //             .expect("lock poisoned")
        //             .try_insert(id, v6_addr);
        //     }
        // }
    }

    async fn handle_query(&mut self, _addr: SocketAddr, _tid: u16, _query: Query) {
        todo!()
    }
}
