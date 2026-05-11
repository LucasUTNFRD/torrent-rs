use crate::{
    error::DhtError,
    message::{CompactNodeInfo, KrpcMessage, MessageBody, Query, Response, TransactionId},
    node_id::NodeId,
    routing_table::{AddressFamily, RoutingTable},
};
use bittorrent_common::types::InfoHash;
use futures::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
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
const K: usize = 8;
#[derive(Debug, Clone)]
pub struct DhtConfig {
    pub port: u16,
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

pub fn default_dht_bootstrap_nodes() -> Vec<BootstrapNode> {
    vec![
        BootstrapNode::new("router.bittorrent.com", 6881),
        BootstrapNode::new("dht.transmissionbt.com", 6881),
        BootstrapNode::new("dht.libtorrent.org", 25401),
        BootstrapNode::new("router.utorrent.com", 6881),
    ]
}

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

    pub fn bootstrap_nodes(mut self, nodes: Vec<BootstrapNode>) -> Self {
        self.bootstrap_nodes = Some(nodes);
        self
    }

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

pub struct Dht {
    tx: mpsc::Sender<DhtCommand>,
}

#[derive(Debug, Clone, Copy)]
pub enum SwarmStatus {
    Broken,
    Poor,
    Firewalled,
    Good,
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
    GetPeers {
        info_hash: InfoHash,
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

        let node_v4 = bind_v4(cfg.port).await.ok().map(|socket| {
            let (tx, rx) = mpsc::channel(64);
            let node = DhtNode::new(node_id, socket, AddressFamily::V4, tx.clone());
            let task = tokio::spawn(async move { node.run(rx).await });
            NodeHandle { tx, task }
        });

        let port = cfg.port;
        let node_v6 = tokio::task::spawn_blocking(move || bind_v6(port))
            .await
            .unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "spawn_blocking failed",
                ))
            })
            .ok()
            .map(|socket| {
                let (tx, rx) = mpsc::channel(64);
                let node = DhtNode::new(node_id, socket, AddressFamily::V6, tx.clone());
                let task = tokio::spawn(async move { node.run(rx).await });
                NodeHandle { tx, task }
            });

        if node_v4.is_none() && node_v6.is_none() {
            return Err(DhtError::IO(
                "Failed to bind either IPv4 or IPv6 socket".to_string(),
            ));
        }

        let coord = DhtCoordinator {
            node_v4,
            node_v6,
            rx: coord_rx,
            wait_bootstrap: Vec::new(),
        };

        tokio::spawn(async move {
            let addrs = resolve_bootstrap(cfg.bootstrap_nodes).await;
            coord.run(addrs).await;
        });

        Ok(Self { tx: coord_tx })
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

#[derive(Debug, Clone)]
struct BootstrapNodeState {
    id: Option<NodeId>,
    addr: SocketAddr,
    queried: bool,
    responded: bool,
}

struct DhtNode {
    node_id: NodeId,
    socket: Arc<UdpSocket>,
    routing_table: RoutingTable,
    family: AddressFamily,
    pending_queries: HashMap<u16, oneshot::Sender<Response>>,
    next_tid: u16,
    tx: mpsc::Sender<DhtNodeCommand>,
}

impl DhtNode {
    fn new(
        node_id: NodeId,
        socket: UdpSocket,
        family: AddressFamily,
        tx: mpsc::Sender<DhtNodeCommand>,
    ) -> Self {
        let routing_table = match family {
            AddressFamily::V4 => RoutingTable::new_v4(node_id),
            AddressFamily::V6 => RoutingTable::new_v6(node_id),
        };
        Self {
            node_id,
            socket: Arc::new(socket),
            routing_table,
            family,
            pending_queries: HashMap::new(),
            next_tid: 0,
            tx,
        }
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

        let mut refresh_interval = tokio::time::interval(Duration::from_secs(15));

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
                _ = refresh_interval.tick() => {
                    self.refresh().await;
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: DhtNodeCommand) {
        match cmd {
            DhtNodeCommand::AddNode { id, addr } => {
                tracing::info!(family = ?self.family, ?id, ?addr, "Adding node to routing table");
                self.routing_table.add_node(id, addr);
            }
            DhtNodeCommand::GetPeers { info_hash, reply } => {
                let node_id = self.node_id;
                let tx = self.tx.clone();
                let family = self.family;
                let target = NodeId::from_slice(info_hash.as_slice()).expect("invalid info_hash");
                let candidates = self.routing_table.find_closest(&target, 8);
                let seed_addrs = candidates.into_iter().map(|n| n.addr).collect();

                tokio::spawn(async move {
                    let peers =
                        Self::get_peers_task(info_hash, seed_addrs, node_id, tx, family).await;
                    let _ = reply.send(peers);
                });
            }
            DhtNodeCommand::Bootstrap { addrs, reply } => {
                let node_id = self.node_id;
                let tx = self.tx.clone();
                let family = self.family;
                tokio::spawn(async move {
                    Self::bootstrap_task(addrs, node_id, tx, family).await;
                    let _ = reply.send(());
                });
            }
            DhtNodeCommand::RoutingTableSize { reply } => {
                let _ = reply.send(self.routing_table.size());
            }
            DhtNodeCommand::QueryNode {
                addr,
                mut query,
                reply,
            } => {
                let tid = self.next_tid();
                // Inject our node ID into the query
                match &mut query {
                    Query::Ping { id } => *id = self.node_id,
                    Query::FindNode { id, .. } => *id = self.node_id,
                    Query::GetPeers { id, .. } => *id = self.node_id,
                    Query::AnnouncePeer { id, .. } => *id = self.node_id,
                }

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

    async fn bootstrap_task(
        bootstrap_addrs: Vec<SocketAddr>,
        our_node_id: NodeId,
        dht_node_tx: mpsc::Sender<DhtNodeCommand>,
        family: AddressFamily,
    ) {
        #[derive(Debug, Clone)]
        struct Cand {
            id: NodeId,
            addr: SocketAddr,
            queried: bool,
            responded: bool,
        }

        let mut candidates: Vec<Cand> = Vec::new();
        let mut seeds: Vec<SocketAddr> = bootstrap_addrs;

        let mut fut = FuturesUnordered::new();
        let mut outstanding = 0;

        loop {
            candidates.sort_by(|a, b| {
                let da = our_node_id.distance(&a.id);
                let db = our_node_id.distance(&b.id);
                da.cmp(&db)
            });

            while outstanding < ALPHA {
                let addr = if let Some(addr) = seeds.pop() {
                    addr
                } else if let Some(node) =
                    candidates.iter_mut().take(K).filter(|n| !n.queried).next()
                {
                    node.queried = true;
                    node.addr
                } else {
                    break;
                };

                outstanding += 1;
                let tx = dht_node_tx.clone();
                let query = Query::FindNode {
                    id: our_node_id,
                    target: our_node_id,
                    is_bootstrap: true,
                    want: Some(match family {
                        AddressFamily::V4 => crate::message::Want::v4_only(),
                        AddressFamily::V6 => crate::message::Want::v6_only(),
                    }),
                };
                fut.push(async move {
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

            if outstanding == 0 {
                break;
            }

            // Why we are using a tokio select?
            tokio::select! {
                Some((addr, res)) = fut.next() => {
                    outstanding -= 1;
                    if let Ok(Ok(Response::FindNode { id, nodes })) = res {
                        if let Some(n) = candidates.iter_mut().find(|n| n.addr == addr) {
                            n.id = id;
                            n.responded = true;
                        } else if !candidates.iter().any(|n| n.addr == addr) {
                            candidates.push(Cand {
                                id,
                                addr,
                                queried: true,
                                responded: true,
                            });
                        }

                        for compact in nodes {
                            let tx = dht_node_tx.clone();
                            let node_id = compact.node_id;
                            let node_addr = compact.addr;
                            tokio::spawn(async move {
                                let _ = tx.send(DhtNodeCommand::AddNode { id: node_id, addr: node_addr }).await;
                            });

                            if !candidates.iter().any(|n| n.addr == node_addr) && !seeds.contains(&node_addr) {
                                candidates.push(Cand {
                                    id: node_id,
                                    addr: node_addr,
                                    queried: false,
                                    responded: false,
                                });
                            }
                        }
                    }
                }
            }

            let responded_count = candidates.iter().filter(|n| n.responded).count();
            if seeds.is_empty() && responded_count >= K {
                let k_closest_queried = candidates
                    .iter()
                    .take(K)
                    .all(|n| n.queried && (n.responded || outstanding == 0));
                if k_closest_queried && outstanding == 0 {
                    break;
                }
            }
        }
    }

    async fn get_peers_task(
        info_hash: InfoHash,
        seed_addrs: Vec<SocketAddr>,
        our_node_id: NodeId,
        dht_node_tx: mpsc::Sender<DhtNodeCommand>,
        family: AddressFamily,
    ) -> Vec<SocketAddr> {
        let mut candidates: Vec<BootstrapNodeState> = seed_addrs
            .into_iter()
            .map(|addr| BootstrapNodeState {
                id: None,
                addr,
                queried: false,
                responded: false,
            })
            .collect();

        let mut fut = FuturesUnordered::new();
        let mut outstanding = 0;
        let mut all_peers = Vec::new();
        let target = NodeId::from_slice(info_hash.as_slice()).expect("invalid info_hash");

        loop {
            candidates.sort_by(|a, b| {
                let da = a.id.map(|id| target.distance(&id));
                let db = b.id.map(|id| target.distance(&id));
                match (da, db) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            });

            while outstanding < ALPHA {
                if let Some(node) = candidates.iter_mut().take(K).filter(|n| !n.queried).next() {
                    node.queried = true;
                    outstanding += 1;
                    let addr = node.addr;
                    let tx = dht_node_tx.clone();
                    let query = Query::GetPeers {
                        id: our_node_id,
                        info_hash,
                        is_bootstrap: false,
                        want: Some(match family {
                            AddressFamily::V4 => crate::message::Want::v4_only(),
                            AddressFamily::V6 => crate::message::Want::v6_only(),
                        }),
                    };

                    fut.push(async move {
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
                } else {
                    break;
                }
            }

            if outstanding == 0 {
                break;
            }

            tokio::select! {
                Some((addr, res)) = fut.next() => {
                    outstanding -= 1;
                    if let Ok(Ok(Response::GetPeers { id, values, nodes, .. })) = res {
                        if let Some(n) = candidates.iter_mut().find(|n| n.addr == addr) {
                            n.id = Some(id);
                            n.responded = true;
                        }

                        if let Some(peers) = values {
                            all_peers.extend(peers);
                        }

                        if let Some(nodes) = nodes {
                            for compact in nodes {
                                if !candidates.iter().any(|n| n.addr == compact.addr) {
                                    candidates.push(BootstrapNodeState {
                                        id: Some(compact.node_id),
                                        addr: compact.addr,
                                        queried: false,
                                        responded: false,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            if all_peers.len() >= 50 {
                break;
            }

            let responded_count = candidates.iter().filter(|n| n.responded).count();
            if responded_count >= K {
                let k_closest_queried = candidates
                    .iter()
                    .take(K)
                    .all(|n| n.queried && (n.responded || outstanding == 0));
                if k_closest_queried && outstanding == 0 {
                    break;
                }
            }
        }

        all_peers
    }

    async fn handle_packet(&mut self, addr: SocketAddr, bytes: Vec<u8>) {
        if let Ok(msg) = KrpcMessage::from_bytes(&bytes) {
            let node_id = msg.get_node_id();
            match msg.body {
                MessageBody::Query(query) => {
                    tracing::info!(family = ?self.family, ?addr, ?query, tid = %msg.transaction_id.as_u16(), "Received query");
                    self.handle_query(addr, msg.transaction_id, query).await;
                }
                MessageBody::Response(response) => {
                    let tid = msg.transaction_id.as_u16();
                    tracing::info!(family = ?self.family, ?addr, ?response, tid, "Received response");
                    if let Some(tx) = self.pending_queries.remove(&tid) {
                        if let Some(id) = node_id {
                            tracing::info!(family = ?self.family, ?id, ?addr, "Updating routing table from response");
                            self.routing_table.add_node(id, addr);
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
            Query::Ping { id } => id,
            Query::FindNode { id, .. } => id,
            Query::GetPeers { id, .. } => id,
            Query::AnnouncePeer { id, .. } => id,
        };
        self.routing_table.add_node(id, addr);

        match query {
            Query::Ping { id: _ } => {
                let msg = KrpcMessage::ping_response(tid, self.node_id);
                let _ = self.socket.send_to(&msg.to_bytes(), addr).await;
            }
            Query::FindNode { target, .. } => {
                let closest = self.routing_table.find_closest(&target, 8);
                let nodes: Vec<CompactNodeInfo> = closest
                    .into_iter()
                    .map(|n| CompactNodeInfo {
                        node_id: n.node_id,
                        addr: n.addr,
                    })
                    .collect();
                let msg = KrpcMessage::find_node_response(tid, self.node_id, nodes);
                let _ = self.socket.send_to(&msg.to_bytes(), addr).await;
            }
            Query::GetPeers {
                id: _,
                info_hash,
                is_bootstrap: _,
                want: _,
            } => {
                let target = NodeId::from_slice(info_hash.as_slice()).expect("invalid info_hash");
                let closest = self.routing_table.find_closest(&target, 8);
                let nodes: Vec<CompactNodeInfo> = closest
                    .into_iter()
                    .map(|n| CompactNodeInfo {
                        node_id: n.node_id,
                        addr: n.addr,
                    })
                    .collect();
                // TODO: Handle tokens and actual peer values if we have them
                let token = vec![1, 2, 3, 4]; // Dummy token
                let msg =
                    KrpcMessage::get_peers_response_with_nodes(tid, self.node_id, token, nodes);
                let _ = self.socket.send_to(&msg.to_bytes(), addr).await;
            }
            _ => {
                // TODO: Handle other queries
            }
        }
    }

    async fn refresh(&mut self) {
        // TODO: Implement refresh logic
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
    rx: mpsc::Receiver<DhtCommand>,
    wait_bootstrap: Vec<oneshot::Sender<()>>,
}

impl DhtCoordinator {
    async fn run(mut self, addrs: Vec<SocketAddr>) {
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

        if let Some(ref node) = self.node_v4 {
            if !v4_addrs.is_empty() {
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
        }
        if let Some(ref node) = self.node_v6 {
            if !v6_addrs.is_empty() {
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
        }

        let mut maintenance = tokio::time::interval(Duration::from_secs(5));
        let mut bootstrapped = false;

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
                _ = maintenance.tick() => {
                    self.maintenance().await;
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
            DhtCommand::FindPeer { info_hash, tx } => {
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
                        .send(DhtNodeCommand::GetPeers {
                            info_hash,
                            reply: reply_tx,
                        })
                        .await;
                    futures.push(reply_rx);
                }
                if let Some(ref node) = self.node_v6 {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = node
                        .tx
                        .send(DhtNodeCommand::GetPeers {
                            info_hash,
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
            DhtCommand::Announce {
                info_hash: _,
                peer_port: _,
                tx,
            } => {
                // For announce, we might want to do GetPeers first to find closer nodes
                // For now, let's just return empty vec or similar
                let _ = tx.send(Vec::new());
            }
            DhtCommand::SwarmStatus { tx } => {
                let status = self.compute_swarm_status().await;
                let _ = tx.send(status);
            }
            DhtCommand::GetRoutingTableSizes { tx } => {
                let mut v4_size = 0;
                let mut v6_size = 0;
                let mut futures = FuturesUnordered::<
                    BoxFuture<'_, (usize, Result<usize, oneshot::error::RecvError>)>,
                >::new();

                if let Some(ref node) = self.node_v4 {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = node
                        .tx
                        .send(DhtNodeCommand::RoutingTableSize { reply: reply_tx })
                        .await;
                    futures.push(Box::pin(async move { (0, reply_rx.await) }));
                }
                if let Some(ref node) = self.node_v6 {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = node
                        .tx
                        .send(DhtNodeCommand::RoutingTableSize { reply: reply_tx })
                        .await;
                    futures.push(Box::pin(async move { (1, reply_rx.await) }));
                }

                while let Some((family, res)) = futures.next().await {
                    if let Ok(size) = res {
                        if family == 0 {
                            v4_size = size;
                        } else {
                            v6_size = size;
                        }
                    }
                }
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

    async fn compute_swarm_status(&self) -> SwarmStatus {
        let mut total_size = 0;
        let mut futures = FuturesUnordered::new();

        if let Some(ref node) = self.node_v4 {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = node
                .tx
                .send(DhtNodeCommand::RoutingTableSize { reply: reply_tx })
                .await;
            futures.push(reply_rx);
        }
        if let Some(ref node) = self.node_v6 {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = node
                .tx
                .send(DhtNodeCommand::RoutingTableSize { reply: reply_tx })
                .await;
            futures.push(reply_rx);
        }

        while let Some(res) = futures.next().await {
            if let Ok(size) = res {
                total_size += size;
            }
        }

        if total_size == 0 {
            SwarmStatus::Broken
        } else if total_size < 20 {
            SwarmStatus::Poor
        } else {
            SwarmStatus::Good
        }
    }

    async fn maintenance(&mut self) {}
}

async fn resolve_bootstrap(bootstrap_nodes: Vec<BootstrapNode>) -> Vec<SocketAddr> {
    let mut futures = FuturesUnordered::new();
    for node in bootstrap_nodes {
        let host = node.host;
        let port = node.port;
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
