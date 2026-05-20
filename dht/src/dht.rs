use crate::{
    dht::tasks::{BootstrapCtx, Search},
    error::DhtError,
    message::{
        CompactNodeInfo, KrpcMessage, MessageBody, Query, Response, TransactionId,
        decode_compact_nodes_v4, decode_compact_nodes_v6, encode_compact_nodes_v4,
        encode_compact_nodes_v6,
    },
    node_id::NodeId,
    peer_store::PeerStore,
    routing_table::{AddressFamily, RoutingTable},
    token::TokenManager,
};
use bencode::{Bencode, BencodeBuilder, BencodeDict};
use bittorrent_common::types::InfoHash;
use futures::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::{BTreeMap, HashMap},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    net::{UdpSocket, lookup_host},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const DEFAULT_PORT: u16 = 6881;
const MAX_PAYLOAD: usize = 2048;

const ALPHA: usize = 5;

/// Well-known bootstrap nodes for the public BitTorrent DHT.
/// Used as a last resort when no cached nodes are available.
const DEFAULT_BOOTSTRAP_HOSTS: &[(&str, u16)] = &[
    ("router.bittorrent.com", 6881),
    ("dht.transmissionbt.com", 6881),
    ("dht.libtorrent.org", 25401),
    ("router.utorrent.com", 6881),
];

/// The fixed filename used for persisted routing table state.
const STATE_FILENAME: &str = "dht.dat";

#[derive(Debug, Clone)]
pub struct DhtConfig {
    pub port: u16,
    /// Directory where the DHT state file (`dht.dat`) is stored.
    /// When set, the DHT will try to load cached nodes from this
    /// directory on startup (skipping DNS bootstrap if successful)
    /// and will write current routing table state here on `shutdown()`.
    pub state_dir: Option<PathBuf>,
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
pub struct DhtConfigBuilder {
    port: u16,
    state_dir: Option<PathBuf>,
}

impl Default for DhtConfigBuilder {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            state_dir: None,
        }
    }
}

impl DhtConfigBuilder {
    pub const fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set the directory where DHT state (`dht.dat`) will be persisted
    /// across restarts. The file is written on `shutdown()` and loaded
    /// automatically on the next startup.
    pub fn state_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_dir = Some(path.into());
        self
    }

    pub fn build(self) -> DhtConfig {
        DhtConfig {
            port: self.port,
            state_dir: self.state_dir,
        }
    }
}

pub struct Dht {
    tx: mpsc::Sender<DhtCommand>,
    rt_v4: Option<Arc<RwLock<RoutingTable>>>,
    rt_v6: Option<Arc<RwLock<RoutingTable>>>,
    state_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum DhtCommand {
    Search {
        info_hash: InfoHash,
        peer_port: Option<u16>,
        tx: oneshot::Sender<Vec<SocketAddr>>,
    },
    AddNode {
        id: NodeId,
        addr: SocketAddr,
    },
    GetRoutingTableSizes {
        tx: oneshot::Sender<(usize, usize)>,
    },
    WaitBootstrap {
        tx: oneshot::Sender<()>,
    },
}

#[derive(Debug)]
pub(crate) enum DhtNodeCommand {
    Bootstrap {
        addrs: Vec<SocketAddr>,
        reply: oneshot::Sender<()>,
    },
    SearchTask {
        info_hash: InfoHash,
        port: Option<u16>,
        reply: oneshot::Sender<Vec<SocketAddr>>,
    },
    AddNode {
        id: NodeId,
        addr: SocketAddr,
    },
    RoutingTableSize {
        reply: oneshot::Sender<usize>,
    },
    QueryNode {
        addr: SocketAddr,
        query: Query,
        reply: oneshot::Sender<Result<Response, DhtError>>,
    },
    TimeoutQuery {
        tid: u16,
    },
}

impl Dht {
    pub async fn new(cfg: DhtConfig) -> Result<Self, DhtError> {
        let (coord_tx, coord_rx) = mpsc::channel(128);
        let node_id = NodeId::random();
        let peer_storage = Arc::new(RwLock::new(PeerStore::default()));

        let s4 = bind_v4(cfg.port).await;
        let s6 = bind_v6(cfg.port);

        let rt_v4 = s4
            .as_ref()
            .ok()
            .map(|_| Arc::new(RwLock::new(RoutingTable::new_v4(node_id))));

        let rt_v6 = s6
            .as_ref()
            .ok()
            .map(|_| Arc::new(RwLock::new(RoutingTable::new_v6(node_id))));

        // ==================================================================
        // Try loading cached nodes from a previous session's dht.dat.
        // If the file exists and parses successfully, we pre-populate the
        // routing tables and use the cached addresses for bootstrapping
        // instead of doing DNS lookups to well-known bootstrap nodes.
        // ==================================================================
        let cached = cfg.state_dir.as_ref().and_then(|dir| {
            match load_cached_nodes(&dir.join(STATE_FILENAME)) {
                Ok(cached) => {
                    tracing::info!(
                        v4 = cached.0.len(),
                        v6 = cached.1.len(),
                        "Loaded cached nodes from dht.dat"
                    );
                    Some(cached)
                }
                Err(e) => {
                    tracing::debug!(?e, "No cached nodes available, will use DNS bootstrap");
                    None
                }
            }
        });

        // Pre-populate routing tables with cached nodes so the bootstrap
        // task starts with a populated table rather than an empty one.
        if let Some((ref v4_nodes, ref v6_nodes)) = cached {
            if let Some(ref rt) = rt_v4 {
                let mut table = rt.write().expect("rt_v4 lock poisoned");
                for node in v4_nodes {
                    table.add_node(node.node_id, node.addr);
                }
            }
            if let Some(ref rt) = rt_v6 {
                let mut table = rt.write().expect("rt_v6 lock poisoned");
                for node in v6_nodes {
                    table.add_node(node.node_id, node.addr);
                }
            }
        }

        let node_v4 = match (s4, rt_v4.clone()) {
            (Ok(socket), Some(rt)) => {
                let (tx, rx) = mpsc::channel(64);
                let node = DhtNode::new(
                    node_id,
                    socket,
                    AddressFamily::V4,
                    rt,
                    rt_v6.clone(),
                    tx.clone(),
                    peer_storage.clone(),
                );
                let task = tokio::spawn(async move { node.run(rx).await });
                Some(NodeHandle { tx, task })
            }
            _ => None,
        };

        let node_v6 = match (s6, rt_v6.clone()) {
            (Ok(socket), Some(rt)) => {
                let (tx, rx) = mpsc::channel(64);
                let node = DhtNode::new(
                    node_id,
                    socket,
                    AddressFamily::V6,
                    rt,
                    rt_v4.clone(),
                    tx.clone(),
                    peer_storage.clone(),
                );
                let task = tokio::spawn(async move { node.run(rx).await });
                Some(NodeHandle { tx, task })
            }
            _ => None,
        };

        if node_v4.is_none() && node_v6.is_none() {
            return Err(DhtError::IO(
                "Failed to bind either IPv4 or IPv6 socket".to_string(),
            ));
        }

        let coord = DhtCoordinator {
            node_v4,
            node_v6,
            rt_v4: rt_v4.clone(),
            rt_v6: rt_v6.clone(),
            rx: coord_rx,
            wait_bootstrap: Vec::new(),
            _peer_storage: peer_storage,
        };

        // Decide bootstrap source: use cached SocketAddrs when available,
        // otherwise resolve well-known DNS bootstrap hosts.
        let use_cached = cached.is_some();
        tokio::spawn(async move {
            let addrs = if use_cached {
                let (v4, v6) = cached.expect("cached is Some when use_cached is true");
                v4.iter().chain(v6.iter()).map(|n| n.addr).collect()
            } else {
                resolve_bootstrap(DEFAULT_BOOTSTRAP_HOSTS).await
            };
            // When starting from cached nodes, the routing tables are
            // already populated so searches can proceed immediately.
            // We still run bootstrap in the background to refresh stale
            // entries and discover nodes closer to our own NodeId.
            coord.run(addrs, use_cached).await;
        });

        Ok(Self {
            tx: coord_tx,
            rt_v4,
            rt_v6,
            state_dir: cfg.state_dir,
        })
    }

    pub async fn announce(
        &self,
        info_hash: InfoHash,
        peer_port: u16,
    ) -> Result<Vec<SocketAddr>, DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::Search {
                info_hash,
                peer_port: Some(peer_port),
                tx,
            })
            .await
            .map_err(|_| DhtError::Cancelled)?;
        rx.await.map_err(|_| DhtError::Cancelled)
    }

    pub async fn find_peers(&self, info_hash: InfoHash) -> Result<Vec<SocketAddr>, DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::Search {
                info_hash,
                peer_port: None,
                tx,
            })
            .await
            .map_err(|_| DhtError::Cancelled)?;
        rx.await.map_err(|_| DhtError::Cancelled)
    }

    pub async fn get_routing_table_sizes(&self) -> Result<(usize, usize), DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::GetRoutingTableSizes { tx })
            .await
            .map_err(|_| DhtError::Cancelled)?;
        rx.await.map_err(|_| DhtError::Cancelled)
    }

    pub async fn wait_bootstrap(&self) -> Result<(), DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::WaitBootstrap { tx })
            .await
            .map_err(|_| DhtError::Cancelled)?;
        rx.await.map_err(|_| DhtError::Cancelled)
    }

    /// Persist the current routing table state to disk and shut down the
    /// DHT actor.
    ///
    /// Call this before dropping the `Dht` handle so that the next
    /// startup can skip DNS-based bootstrap and resume from cached
    /// nodes. Without calling this, the DHT will need to re-bootstrap
    /// from scratch on the next run.
    pub async fn shutdown(self) -> Result<(), DhtError> {
        if let Some(ref dir) = self.state_dir {
            persist_routing_tables(
                &dir.join(STATE_FILENAME),
                self.rt_v4.as_ref(),
                self.rt_v6.as_ref(),
            )?;
        }
        // Dropping `self` (and therefore `self.tx`) signals the
        // coordinator to stop its event loop.
        Ok(())
    }
}

async fn bind_v4(port: u16) -> tokio::io::Result<UdpSocket> {
    UdpSocket::bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).await
}

fn bind_v6(port: u16) -> tokio::io::Result<UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    socket.set_only_v6(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), port).into())?;
    UdpSocket::from_std(socket.into())
}

const REFRESH_TABLE_INTERVAL: Duration = Duration::from_secs(15 * 60);
const PING_TABLE_INTERVAL: Duration = Duration::from_secs(5 * 60);

struct DhtNode {
    node_id: NodeId,
    socket: Arc<UdpSocket>,
    routing_table: Arc<RwLock<RoutingTable>>,
    remote_routing_table: Option<Arc<RwLock<RoutingTable>>>,
    family: AddressFamily,
    pending_queries: HashMap<u16, oneshot::Sender<Response>>,
    next_tid: u16,
    tx: mpsc::Sender<DhtNodeCommand>,
    peer_storage: Arc<RwLock<PeerStore>>,
    token_manager: TokenManager,
}

impl DhtNode {
    #[allow(clippy::too_many_arguments)]
    fn new(
        node_id: NodeId,
        socket: UdpSocket,
        family: AddressFamily,
        routing_table: Arc<RwLock<RoutingTable>>,
        remote_routing_table: Option<Arc<RwLock<RoutingTable>>>,
        tx: mpsc::Sender<DhtNodeCommand>,
        peer_storage: Arc<RwLock<PeerStore>>,
    ) -> Self {
        Self {
            node_id,
            socket: Arc::new(socket),
            routing_table,
            remote_routing_table,
            family,
            pending_queries: HashMap::new(),
            next_tid: 0,
            tx,
            peer_storage,
            token_manager: TokenManager::new(Duration::from_secs(15 * 60)),
        }
    }

    fn get_nodes_for_target(
        &self,
        target: &NodeId,
        want: Option<crate::message::Want>,
    ) -> Vec<CompactNodeInfo> {
        let want = want.unwrap_or_else(|| match self.family {
            AddressFamily::V4 => crate::message::Want::v4_only(),
            AddressFamily::V6 => crate::message::Want::v6_only(),
        });

        let mut nodes = Vec::new();

        if want.n4 {
            let rt = if self.family == AddressFamily::V4 {
                Some(&self.routing_table)
            } else {
                self.remote_routing_table.as_ref()
            };
            if let Some(rt) = rt {
                let closest = rt.read().unwrap().find_closest(target, 8);
                nodes.extend(closest.into_iter().map(|n| CompactNodeInfo {
                    node_id: n.node_id,
                    addr: n.addr,
                }));
            }
        }

        if want.n6 {
            let rt = if self.family == AddressFamily::V6 {
                Some(&self.routing_table)
            } else {
                self.remote_routing_table.as_ref()
            };
            if let Some(rt) = rt {
                let closest = rt.read().unwrap().find_closest(target, 8);
                nodes.extend(closest.into_iter().map(|n| CompactNodeInfo {
                    node_id: n.node_id,
                    addr: n.addr,
                }));
            }
        }

        nodes
    }

    const fn next_tid(&mut self) -> u16 {
        let tid = self.next_tid;
        self.next_tid = self.next_tid.wrapping_add(1);
        tid
    }

    async fn run(mut self, mut cmd_rx: mpsc::Receiver<DhtNodeCommand>) {
        let (packet_tx, mut packet_rx) = mpsc::channel(64);
        let socket_clone = self.socket.clone();
        tokio::spawn(async move {
            let _ = run_socket(socket_clone, packet_tx).await;
        });

        let mut last_table_ping = tokio::time::interval(PING_TABLE_INTERVAL);
        let mut last_table_refresh = tokio::time::interval(REFRESH_TABLE_INTERVAL);
        last_table_ping.tick().await;
        last_table_refresh.tick().await;

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await,
                        None => break,
                    }
                }
                packet = packet_rx.recv() => {
                    match packet {
                        Some((addr, bytes)) => self.handle_packet(addr, bytes).await,
                        None => break,
                    }
                }
                _ = last_table_ping.tick() => {
                    self.run_periodic_ping().await;
                }
                _ = last_table_refresh.tick() => {
                    self.refresh_table().await;
                }
            }
        }
    }

    async fn refresh_table(&self) {
        // It is basically run a find node task
    }

    async fn run_periodic_ping(&self) {}

    async fn handle_command(&mut self, cmd: DhtNodeCommand) {
        match cmd {
            DhtNodeCommand::AddNode { id, addr } => {
                // tracing::info!(family = ?self.family, ?id, ?addr, "Adding node to routing table");
                self.routing_table.write().unwrap().add_node(id, addr);
            }
            DhtNodeCommand::SearchTask {
                info_hash,
                port,
                reply,
            } => {
                let our_node_id = self.node_id;
                let dht_node_tx = self.tx.clone();
                let af = self.family;
                let target = NodeId::from_slice(info_hash.as_slice()).expect("invalid info_hash");
                let k_closest_nodes = self.routing_table.read().unwrap().find_closest(&target, 8);
                let jh = Search::start_search_task(
                    target,
                    our_node_id,
                    dht_node_tx,
                    port,
                    af,
                    &k_closest_nodes,
                );
                tokio::spawn(async move {
                    let adrs = jh.await.unwrap();
                    let _ = reply.send(adrs);
                });
            }
            DhtNodeCommand::Bootstrap { addrs, reply } => {
                let node_id = self.node_id;
                let tx = self.tx.clone();
                let family = self.family;
                let jh = BootstrapCtx::start_boostrap(addrs, node_id, tx, family);
                tokio::spawn(async move {
                    let _ = jh.await;
                    let _ = reply.send(());
                });
            }
            DhtNodeCommand::RoutingTableSize { reply } => {
                let _ = reply.send(self.routing_table.read().unwrap().size());
            }
            DhtNodeCommand::QueryNode { addr, query, reply } => {
                let tid = self.next_tid();

                tracing::debug!(family = ?self.family, ?addr, ?query, tid, "Sending query");

                let msg = KrpcMessage {
                    transaction_id: TransactionId::new(tid),
                    version: None,
                    sender_ip: None,
                    body: MessageBody::Query(query),
                };

                let (tx, rx) = oneshot::channel();
                self.pending_queries.insert(tid, tx);

                if self.socket.send_to(&msg.to_bytes(), addr).await.is_err() {
                    self.pending_queries.remove(&tid);
                    let _ = reply.send(Err(DhtError::IO("Failed to send packet".to_string())));
                    return;
                }

                let tx_clone = self.tx.clone();
                tokio::spawn(async move {
                    // Timeout for queries
                    let res = tokio::time::timeout(Duration::from_secs(5), rx).await;
                    match res {
                        Ok(Ok(resp)) => {
                            let _ = reply.send(Ok(resp));
                        }
                        Ok(Err(_)) => {
                            // Channel closed
                        }
                        Err(_) => {
                            let _ = tx_clone.send(DhtNodeCommand::TimeoutQuery { tid }).await;
                            let _ = reply.send(Err(DhtError::Timeout));
                        }
                    }
                });
            }
            DhtNodeCommand::TimeoutQuery { tid } => {
                self.pending_queries.remove(&tid);
            }
        }
    }

    async fn handle_packet(&mut self, addr: SocketAddr, bytes: Vec<u8>) {
        if let Ok(msg) = KrpcMessage::from_bytes(&bytes) {
            let node_id = msg.get_node_id();
            match msg.body {
                MessageBody::Query(query) => {
                    // tracing::info!(family = ?self.family, ?addr, ?query, tid = %msg.transaction_id.as_u16(), "Received query");
                    self.handle_query(addr, msg.transaction_id, query).await;
                }
                MessageBody::Response(response) => {
                    let tid = msg.transaction_id.as_u16();
                    // tracing::info!(family = ?self.family, ?addr, ?response, tid, "Received response");
                    if let Some(tx) = self.pending_queries.remove(&tid) {
                        if let Some(id) = node_id {
                            // tracing::info!(family = ?self.family, ?id, ?addr, "Updating routing table from response");
                            self.routing_table.write().unwrap().add_node(id, addr);
                        }
                        let _ = tx.send(response);
                    }
                }
                MessageBody::Error { code, ref message } => {
                    tracing::warn!(family = ?self.family, ?addr, code, message, "Received KRPC error");
                }
            }
        }
    }

    async fn handle_query(&mut self, addr: SocketAddr, tid: TransactionId, query: Query) {
        // BEP 5: Add querying node to routing table
        let id = match query {
            Query::Ping { id }
            | Query::FindNode { id, .. }
            | Query::GetPeers { id, .. }
            | Query::AnnouncePeer { id, .. } => id,
        };

        self.routing_table.write().unwrap().add_node(id, addr);

        match query {
            Query::Ping { id: _ } => {
                let msg = KrpcMessage::ping_response(tid, self.node_id);
                let _ = self.socket.send_to(&msg.to_bytes(), addr).await;
            }
            Query::FindNode {
                id: _,
                target,
                is_bootstrap: _,
                want,
            } => {
                let nodes = self.get_nodes_for_target(&target, want);
                let msg = KrpcMessage::find_node_response(tid, self.node_id, nodes);
                let _ = self.socket.send_to(&msg.to_bytes(), addr).await;
            }
            Query::GetPeers {
                id: _,
                info_hash,
                is_bootstrap: _,
                want,
            } => {
                let token = self
                    .token_manager
                    .generate_token(&addr.ip(), &info_hash)
                    .to_vec();
                let peers = self.peer_storage.read().unwrap().get_peers(&info_hash);

                let filtered_peers = peers
                    .map(|p| {
                        p.into_iter()
                            .filter(|addr| match self.family {
                                AddressFamily::V4 => addr.is_ipv4(),
                                AddressFamily::V6 => addr.is_ipv6(),
                            })
                            .collect::<Vec<_>>()
                    })
                    .filter(|p: &Vec<SocketAddr>| !p.is_empty());

                let target = NodeId::from_slice(info_hash.as_bytes()).expect("invalid info_hash");
                let nodes = self.get_nodes_for_target(&target, want);

                let nodes_opt = if nodes.is_empty() { None } else { Some(nodes) };

                let msg = KrpcMessage::get_peers_response(
                    tid,
                    self.node_id,
                    token,
                    filtered_peers,
                    nodes_opt,
                );

                let _ = self.socket.send_to(&msg.to_bytes(), addr).await;
            }
            Query::AnnouncePeer {
                id: _,
                info_hash,
                port,
                token,
                implied_port,
            } => {
                // Ensure secrets rotate if necessary before validation
                self.token_manager.check_rotation();

                if !self.token_manager.validate_token(
                    &addr.ip(),
                    &info_hash,
                    token.try_into().expect("It must be a u8;20)"),
                ) {
                    // BEP 5 Protocol Error: 203 Protocol Error (or Malicious/Expired token)
                    let err_msg = KrpcMessage::error_response(
                        tid,
                        203,
                        "Invalid or expired token".to_string(),
                    );
                    let _ = self.socket.send_to(&err_msg.to_bytes(), addr).await;
                    return;
                }

                let peer_port = if implied_port { addr.port() } else { port };

                let remote_peer_addr = SocketAddr::new(addr.ip(), peer_port);

                self.peer_storage
                    .write()
                    .unwrap()
                    .add_peer(info_hash, remote_peer_addr);

                let msg = KrpcMessage::announce_peer_response(tid, self.node_id);
                let _ = self.socket.send_to(&msg.to_bytes(), addr).await;
            }
        }
    }
}

async fn run_socket(
    socket: Arc<UdpSocket>,
    tx: mpsc::Sender<(SocketAddr, Vec<u8>)>,
) -> Result<(), DhtError> {
    let mut buf = vec![0u8; MAX_PAYLOAD];

    loop {
        let (len, addr) = socket.recv_from(&mut buf).await?;
        let bytes = buf[..len].to_vec();

        if tx.send((addr, bytes)).await.is_err() {
            break;
        }
    }

    Ok(())
}

struct NodeHandle {
    tx: mpsc::Sender<DhtNodeCommand>,
    task: JoinHandle<()>,
}

impl Drop for NodeHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct DhtCoordinator {
    node_v4: Option<NodeHandle>,
    node_v6: Option<NodeHandle>,
    rt_v4: Option<Arc<RwLock<RoutingTable>>>,
    rt_v6: Option<Arc<RwLock<RoutingTable>>>,
    rx: mpsc::Receiver<DhtCommand>,
    wait_bootstrap: Vec<oneshot::Sender<()>>,
    _peer_storage: Arc<RwLock<PeerStore>>,
}

impl DhtCoordinator {
    async fn run(mut self, addrs: Vec<SocketAddr>, pre_bootstrapped: bool) {
        let mut v4_bootstrap = None;
        let mut v6_bootstrap = None;

        let mut v4_addrs = Vec::new();
        let mut v6_addrs = Vec::new();

        for addr in addrs {
            if addr.is_ipv4() {
                v4_addrs.push(addr);
            } else {
                v6_addrs.push(addr);
            }
        }

        if !v4_addrs.is_empty()
            && let Some(ref node) = self.node_v4
        {
            let (tx, rx) = oneshot::channel();
            let _ = node
                .tx
                .send(DhtNodeCommand::Bootstrap {
                    addrs: v4_addrs,
                    reply: tx,
                })
                .await;
            v4_bootstrap = Some(rx);
        }

        if !v6_addrs.is_empty()
            && let Some(ref node) = self.node_v6
        {
            let (tx, rx) = oneshot::channel();
            let _ = node
                .tx
                .send(DhtNodeCommand::Bootstrap {
                    addrs: v6_addrs,
                    reply: tx,
                })
                .await;
            v6_bootstrap = Some(rx);
        }

        // When starting from cached nodes (`pre_bootstrapped = true`), the
        // routing tables are already populated so we treat the DHT as ready
        // immediately. The bootstrap tasks still run in the background to
        // verify liveness and discover closer nodes, but they don't block
        // `wait_bootstrap()` or `get_peers()` callers.
        //
        // Also handle the edge case where no bootstrap tasks were spawned at
        // all (empty addrs, or addrs didn't match any bound address family).
        let mut bootstrapped =
            pre_bootstrapped || (v4_bootstrap.is_none() && v6_bootstrap.is_none());

        if bootstrapped {
            tracing::info!(
                from_cache = pre_bootstrapped,
                "DHT ready (bootstrap {})",
                if pre_bootstrapped {
                    "running in background"
                } else {
                    "skipped"
                }
            );
            for tx in self.wait_bootstrap.drain(..) {
                let _ = tx.send(());
            }
        }

        loop {
            tokio::select! {
                _res = async {
                    if let Some(rx) = v4_bootstrap.as_mut() {
                        rx.await.ok()
                    } else {
                        futures::future::pending().await
                    }
                }, if v4_bootstrap.is_some() => {
                    v4_bootstrap = None;
                    tracing::info!("IPv4 node finished bootstrapping");
                    if v4_bootstrap.is_none() && v6_bootstrap.is_none() {
                        bootstrapped = true;
                        tracing::info!("DHT bootstrap complete");
                        for tx in self.wait_bootstrap.drain(..) {
                            let _ = tx.send(());
                        }
                    }
                }
                _res = async {
                    if let Some(rx) = v6_bootstrap.as_mut() {
                        rx.await.ok()
                    } else {
                        futures::future::pending().await
                    }
                }, if v6_bootstrap.is_some() => {
                    v6_bootstrap = None;
                    tracing::info!("IPv6 node finished bootstrapping");
                    if v4_bootstrap.is_none() && v6_bootstrap.is_none() {
                        bootstrapped = true;
                        tracing::info!("DHT bootstrap complete");
                        for tx in self.wait_bootstrap.drain(..) {
                            let _ = tx.send(());
                        }
                    }
                }
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd, bootstrapped).await,
                        None => break,
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: DhtCommand, bootstrapped: bool) {
        match cmd {
            DhtCommand::AddNode { id, addr } => {
                let target_node = if addr.is_ipv4() {
                    &self.node_v4
                } else {
                    &self.node_v6
                };
                if let Some(node) = target_node {
                    let _ = node.tx.send(DhtNodeCommand::AddNode { id, addr }).await;
                }
            }
            DhtCommand::Search {
                info_hash,
                tx,
                peer_port,
            } => {
                if !bootstrapped {
                    tracing::warn!(
                        "FindPeer requested before bootstrap complete - results may be limited"
                    );
                }
                let mut futures = FuturesUnordered::new();
                if let Some(ref node) = self.node_v4 {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = node
                        .tx
                        .send(DhtNodeCommand::SearchTask {
                            info_hash,
                            port: peer_port,
                            reply: reply_tx,
                        })
                        .await;
                    futures.push(reply_rx);
                }
                if let Some(ref node) = self.node_v6 {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = node
                        .tx
                        .send(DhtNodeCommand::SearchTask {
                            info_hash,
                            port: peer_port,
                            reply: reply_tx,
                        })
                        .await;
                    futures.push(reply_rx);
                }

                tokio::spawn(async move {
                    let mut all_peers = Vec::new();
                    while let Some(res) = futures.next().await {
                        if let Ok(peers) = res {
                            all_peers.extend(peers);
                        }
                    }
                    let _ = tx.send(all_peers);
                });
            }
            DhtCommand::GetRoutingTableSizes { tx } => {
                let v4_size = self
                    .rt_v4
                    .as_ref()
                    .map_or(0, |rt| rt.read().unwrap().size());
                let v6_size = self
                    .rt_v6
                    .as_ref()
                    .map_or(0, |rt| rt.read().unwrap().size());
                let _ = tx.send((v4_size, v6_size));
            }
            DhtCommand::WaitBootstrap { tx } => {
                if bootstrapped {
                    let _ = tx.send(());
                } else {
                    self.wait_bootstrap.push(tx);
                }
            }
        }
    }
}

async fn resolve_bootstrap(hosts: &[(&str, u16)]) -> Vec<SocketAddr> {
    let mut futures = FuturesUnordered::new();
    for &(host, port) in hosts {
        futures.push(async move {
            let res = lookup_host(format!("{host}:{port}"))
                .await
                .map(std::iter::Iterator::collect::<Vec<_>>)
                .unwrap_or_default();
            (host, res)
        });
    }

    let mut addrs = Vec::new();
    while let Some((host, mut host_addrs)) = futures.next().await {
        if host_addrs.is_empty() {
            tracing::warn!(?host, "Failed to resolve bootstrap node");
        } else {
            tracing::info!(?host, count = host_addrs.len(), "Resolved bootstrap node");
        }
        addrs.append(&mut host_addrs);
    }
    addrs
}

// ============================================================================
// Routing table persistence
// ============================================================================

/// Persist both routing tables as bencoded compact nodes to the state file.
///
/// File format (bencode dict):
///   n4: <compact IPv4 nodes — 26 bytes each: 20-byte ``NodeId`` + 4-byte IP + 2-byte port>
///   n6: <compact IPv6 nodes — 38 bytes each: 20-byte ``NodeId`` + 16-byte IP + 2-byte port>
fn persist_routing_tables(
    path: &Path,
    rt_v4: Option<&Arc<RwLock<RoutingTable>>>,
    rt_v6: Option<&Arc<RwLock<RoutingTable>>>,
) -> Result<(), DhtError> {
    let mut dict = BTreeMap::<Vec<u8>, Bencode>::new();

    if let Some(rt) = rt_v4 {
        let nodes: Vec<CompactNodeInfo> = rt
            .read()
            .expect("rt_v4 lock poisoned")
            .iter()
            .map(|entry| CompactNodeInfo {
                node_id: entry.node_id,
                addr: entry.addr,
            })
            .collect();
        dict.put("n4", &encode_compact_nodes_v4(&nodes).as_slice());
    }

    if let Some(rt) = rt_v6 {
        let nodes: Vec<CompactNodeInfo> = rt
            .read()
            .expect("rt_v6 lock poisoned")
            .iter()
            .map(|entry| CompactNodeInfo {
                node_id: entry.node_id,
                addr: entry.addr,
            })
            .collect();
        dict.put("n6", &encode_compact_nodes_v6(&nodes).as_slice());
    }

    let bytes = Bencode::encoder(&dict.build());

    // Ensure the parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            DhtError::IO(format!("create state directory {}: {e}", parent.display()))
        })?;
    }

    std::fs::write(path, bytes)
        .map_err(|e| DhtError::IO(format!("write state file {}: {e}", path.display())))?;

    tracing::info!(path = %path.display(), "Persisted DHT routing tables");
    Ok(())
}

/// Try to load cached nodes from a bencoded state file.
///
/// Returns `(v4_nodes, v6_nodes)`. Either list may be empty if the
/// corresponding key is missing from the file.
fn load_cached_nodes(
    path: &Path,
) -> Result<(Vec<CompactNodeInfo>, Vec<CompactNodeInfo>), DhtError> {
    let bytes = std::fs::read(path)
        .map_err(|e| DhtError::IO(format!("read state file {}: {e}", path.display())))?;

    let bencode = Bencode::decode(&bytes)?;
    let Bencode::Dict(dict) = bencode else {
        return Err(DhtError::Parse("dht.dat: expected dictionary".to_string()));
    };

    let v4_nodes = dict
        .get_bytes(b"n4")
        .map(decode_compact_nodes_v4)
        .transpose()?
        .unwrap_or_default();

    let v6_nodes = dict
        .get_bytes(b"n6")
        .map(decode_compact_nodes_v6)
        .transpose()?
        .unwrap_or_default();

    Ok((v4_nodes, v6_nodes))
}

mod tasks {
    use crate::{
        dht::{ALPHA, DhtNodeCommand},
        message::{Query, Response, Want},
        node_id::NodeId,
        routing_table::{AddressFamily, K, NodeEntry},
    };
    use bittorrent_common::types::InfoHash;
    use futures::{StreamExt, future::join_all, stream::FuturesUnordered};
    use std::{
        collections::{BTreeMap, HashSet},
        net::SocketAddr,
        time::Duration,
    };
    use tokio::{
        sync::{mpsc, oneshot},
        task::JoinHandle,
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum NodeState {
        Unqueried,
        InFlight,
        Responded,
        Failed,
    }

    #[derive(Clone)]
    struct CandidateNode {
        // None when we start from just SocketAddr
        id: Option<NodeId>,
        addr: SocketAddr,
        state: NodeState,
        token: Option<Vec<u8>>,
    }

    type DistanceToTarget = NodeId;
    struct ClosestNodes {
        target: NodeId,
        nodes: BTreeMap<DistanceToTarget, CandidateNode>,
        seen: HashSet<SocketAddr>,
    }

    impl ClosestNodes {
        fn new(target: NodeId) -> Self {
            Self {
                target,
                nodes: BTreeMap::new(),
                seen: HashSet::new(),
            }
        }

        fn insert(&mut self, id: NodeId, addr: SocketAddr) {
            if self.seen.insert(addr) {
                let distance = self.target ^ id;
                self.nodes.insert(
                    distance,
                    CandidateNode {
                        id: Some(id),
                        addr,
                        state: NodeState::Unqueried,
                        token: None,
                    },
                );
            }
        }

        fn get_next_unqueried(&mut self, count: usize) -> Vec<SocketAddr> {
            let mut next = Vec::new();

            for node in self.nodes.values_mut() {
                if node.state == NodeState::Unqueried {
                    node.state = NodeState::InFlight;
                    next.push(node.addr);
                    if next.len() == count {
                        break;
                    }
                }
            }

            next
        }

        fn update_state(&mut self, addr: SocketAddr, state: NodeState, token: Option<Vec<u8>>) {
            if let Some(node) = self.nodes.values_mut().find(|n| n.addr == addr) {
                node.state = state;
                if token.is_some() {
                    node.token = token;
                }
            }
        }

        fn is_complete(&self) -> bool {
            let mut responded_count = 0;
            let mut in_flight_count = 0;

            let top_k_nodes: Vec<_> = self.nodes.values().take(K).collect();

            for node in &top_k_nodes {
                match node.state {
                    NodeState::Responded => responded_count += 1,
                    NodeState::InFlight => in_flight_count += 1,
                    _ => {}
                }
            }

            // 1. Condition A: We found K good nodes and no better candidates are pending
            let reached_full_k = responded_count >= K && in_flight_count == 0;

            let is_processed = |n: &&CandidateNode| {
                n.state == NodeState::Responded || n.state == NodeState::Failed
            };

            // 2. Condition B: We've exhausted all possible candidates among the top K
            // (This handles cases where we know fewer than K nodes total)
            let exhausted_top_k = top_k_nodes.iter().all(is_processed);

            reached_full_k || exhausted_top_k
        }

        fn get_k_closest_responded(&self) -> Vec<(SocketAddr, NodeId, Vec<u8>)> {
            self.nodes
                .values()
                .filter(|n| n.state == NodeState::Responded && n.token.is_some() && n.id.is_some())
                .take(K)
                .map(|n| (n.addr, n.id.unwrap(), n.token.clone().unwrap()))
                .collect()
        }
    }

    pub(super) struct Search {
        target: NodeId,
        our_node_id: NodeId,
        dht_node_tx: mpsc::Sender<DhtNodeCommand>,
        announce_port: Option<u16>,
        af: AddressFamily,
        candidates: ClosestNodes,
        discovered_peers: HashSet<SocketAddr>,
    }

    impl Search {
        pub(super) fn start_search_task(
            target: NodeId,
            our_node_id: NodeId,
            dht_node_tx: mpsc::Sender<DhtNodeCommand>,
            announce_port: Option<u16>,
            af: AddressFamily,
            initial_nodes: &[NodeEntry],
        ) -> tokio::task::JoinHandle<Vec<SocketAddr>> {
            let mut candidates = ClosestNodes::new(target);
            for node in initial_nodes {
                let addr = node.addr;
                let id = node.node_id;
                candidates.insert(id, addr);
            }

            let search = Self {
                target,
                our_node_id,
                dht_node_tx,
                announce_port,
                af,
                candidates,
                discovered_peers: HashSet::default(),
            };

            tokio::spawn(async move { search.run().await })
        }

        async fn run(mut self) -> Vec<SocketAddr> {
            let mut futs = FuturesUnordered::new();

            loop {
                let next_queries = self.candidates.get_next_unqueried(ALPHA - futs.len());
                for addr in next_queries {
                    let tx = self.dht_node_tx.clone();
                    let query = Query::GetPeers {
                        id: self.our_node_id,
                        info_hash: InfoHash::from_slice(self.target.as_bytes()).unwrap(),
                        is_bootstrap: false,
                        want: Some(match self.af {
                            AddressFamily::V4 => Want::v4_only(),
                            AddressFamily::V6 => Want::v6_only(),
                        }),
                    };

                    futs.push(async move {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let _ = tx
                            .send(DhtNodeCommand::QueryNode {
                                addr,
                                query,
                                reply: reply_tx,
                            })
                            .await;
                        (addr, reply_rx.await)
                    });
                }

                if futs.is_empty() {
                    break;
                }

                tokio::select! {
                    Some((addr, res)) = futs.next() => {
                        match res {
                            Ok(Ok(Response::GetPeers { id:_, values, nodes, token, .. })) => {
                                self.candidates.update_state(addr, NodeState::Responded, Some(token));
                                if let Some(peers) = values {
                                    for peer in peers {
                                        self.discovered_peers.insert(peer);
                                    }
                                }
                                if let Some(nodes) = nodes {
                                    for node in nodes {
                                        self.candidates.insert(node.node_id, node.addr);
                                    }
                                }
                            }
                            _ => {
                                self.candidates.update_state(addr, NodeState::Failed, None);
                            }
                        }
                    }
                }

                if self.candidates.is_complete() {
                    break;
                }
            }

            //if non-zero, this search is intended to result in an announce_peer. Once the closest nodes are found
            //via get_peers, the search transitions into an announce phase.
            if let Some(port) = self.announce_port
                && port != 0
            {
                let closest = self.candidates.get_k_closest_responded();
                let mut announce_futs = Vec::with_capacity(closest.len());

                for (addr, _node_id, token) in closest {
                    let tx = self.dht_node_tx.clone();
                    let query = Query::AnnouncePeer {
                        id: self.our_node_id,
                        info_hash: InfoHash::from_slice(self.target.as_bytes()).unwrap(),
                        port,
                        token,
                        implied_port: true,
                    };

                    announce_futs.push(async move {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let _ = tx
                            .send(DhtNodeCommand::QueryNode {
                                addr,
                                query,
                                reply: reply_tx,
                            })
                            .await;
                        (addr, reply_rx.await)
                    });
                }

                // Best-Effort of announce to k closest responsive nodes of the get_peers phase
                let results = join_all(announce_futs).await;
                for (addr, res) in results {
                    if let Ok(Ok(Response::AnnouncePeer { .. })) = res {
                        tracing::info!(target: "dht_search", %addr, "Successfully announced peer to node");
                    } else {
                        tracing::warn!(target: "dht_search", %addr, "Received unexpected response variant for AnnouncePeer");
                    }
                }
            }

            self.discovered_peers.iter().copied().collect()
        }
    }

    pub(super) struct BootstrapCtx {
        bootstrap_addrs: Vec<SocketAddr>,
        our_node_id: NodeId,
        dht_node_tx: mpsc::Sender<DhtNodeCommand>,
        family: AddressFamily,
        candidate: ClosestNodes,
    }

    // Maybe refactor this into wide abstraction on top of FindNode.
    // Where a special case is running a boostrap or the other is juts a FindNode task?

    impl BootstrapCtx {
        pub(super) fn start_boostrap(
            bootstrap_addrs: Vec<SocketAddr>,
            our_node_id: NodeId,
            dht_node_tx: mpsc::Sender<DhtNodeCommand>,
            family: AddressFamily,
        ) -> JoinHandle<()> {
            let boostrap = Self {
                bootstrap_addrs,
                our_node_id,
                dht_node_tx,
                family,
                candidate: ClosestNodes::new(our_node_id),
            };
            tokio::spawn(async move {
                boostrap.run().await;
            })
        }

        async fn run(mut self) {
            let mut futs = FuturesUnordered::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

            loop {
                // Fill the in-flight pipeline up to ALPHA.
                // Seeds are consumed first; once exhausted, pull unqueried candidates
                // from ClosestNodes (already XOR-sorted by BTreeMap key, no sort needed).
                while futs.len() < ALPHA {
                    let addr = if let Some(addr) = self.bootstrap_addrs.pop() {
                        addr
                    } else {
                        match self.candidate.get_next_unqueried(1).into_iter().next() {
                            Some(addr) => addr,
                            None => break,
                        }
                    };

                    let tx = self.dht_node_tx.clone();
                    let our_node_id = self.our_node_id;
                    let family = self.family;

                    futs.push(async move {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let query = Query::FindNode {
                            id: our_node_id,
                            target: our_node_id,
                            is_bootstrap: true,
                            want: Some(match family {
                                AddressFamily::V4 => Want::v4_only(),
                                AddressFamily::V6 => Want::v6_only(),
                            }),
                        };
                        let _ = tx
                            .send(DhtNodeCommand::QueryNode {
                                addr,
                                query,
                                reply: reply_tx,
                            })
                            .await;
                        (addr, reply_rx.await)
                    });
                }

                if futs.is_empty() {
                    break;
                }

                tokio::select! {
                    Some((addr, res)) = futs.next() => {
                        match res {
                            Ok(Ok(Response::FindNode { id, nodes })) => {
                                // Now that we have the real NodeId, insert at correct XOR distance.
                                // For ClosestNodes candidates this is a seen-dedup no-op on the addr.
                                self.candidate.insert(id, addr);
                                self.candidate.update_state(addr, NodeState::Responded, None);

                                for compact in nodes {
                                    let _ = self
                                        .dht_node_tx
                                        .send(DhtNodeCommand::AddNode {
                                            id: compact.node_id,
                                            addr: compact.addr,
                                        })
                                        .await;
                                    // ClosestNodes::seen deduplicates across all inserts
                                    self.candidate.insert(compact.node_id, compact.addr);
                                }
                            }
                            _ => {
                                // Seeds not yet in ClosestNodes: no-op — correct, they just disappear.
                                // ClosestNodes candidates: marks Failed, won't be re-queried.
                                self.candidate.update_state(addr, NodeState::Failed, None);
                            }
                        }
                    }
                    () = tokio::time::sleep_until(deadline) => {
                        tracing::warn!(family = ?self.family, "Bootstrap timed out after 30s");
                        break;
                    }
                }

                // Only check completion once seeds are fully drained —
                // ClosestNodes::is_complete() can't account for addr-only seeds.
                if self.bootstrap_addrs.is_empty() && self.candidate.is_complete() {
                    break;
                }
            }
        }
    }
}
