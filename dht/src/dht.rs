use crate::{
    error::DhtError,
    message::{KrpcMessage, MessageBody, Query, Response, TransactionId},
    node_id::NodeId,
    routing_table::{AddressFamily, RoutingTable},
};
use bittorrent_common::types::InfoHash;
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
    task::JoinHandle,
};

const DEFAULT_PORT: u16 = 6881;
const MAX_PAYLOAD: usize = 2048;

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
}

#[derive(Debug)]
pub(crate) enum DhtNodeCommand {
    Bootstrap {
        addrs: Vec<SocketAddr>,
        // SOmething to notify task done
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
}

impl Dht {
    pub async fn new(cfg: DhtConfig) -> Result<Self, DhtError> {
        let (coord_tx, coord_rx) = mpsc::channel(128);
        let node_id = NodeId::random();

        let node_v4 = bind_v4(cfg.port).await.ok().map(|socket| {
            let (tx, rx) = mpsc::channel(64);
            let node = DhtNode::new(node_id, socket, AddressFamily::V4);
            let task = tokio::spawn(async move { node.run(rx).await });
            NodeHandle { tx, task }
        });

        let node_v6 = bind_v6(cfg.port).await.ok().map(|socket| {
            let (tx, rx) = mpsc::channel(64);
            let node = DhtNode::new(node_id, socket, AddressFamily::V6);
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
        };

        tokio::spawn(async move {
            let addrs = resolve_bootstrap(cfg.bootstrap_nodes).await;
            coord.bootstrap(addrs).await;
            coord.run().await;
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
}

async fn bind_v4(port: u16) -> tokio::io::Result<UdpSocket> {
    UdpSocket::bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).await
}

async fn bind_v6(port: u16) -> tokio::io::Result<UdpSocket> {
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

struct DhtNode {
    node_id: NodeId,
    socket: Arc<UdpSocket>,
    routing_table: RoutingTable,
    pending_queries: HashMap<u16, oneshot::Sender<Response>>,
    next_tid: u16,
}

impl DhtNode {
    fn new(node_id: NodeId, socket: UdpSocket, family: AddressFamily) -> Self {
        let routing_table = match family {
            AddressFamily::V4 => RoutingTable::new_v4(node_id),
            AddressFamily::V6 => RoutingTable::new_v6(node_id),
        };
        Self {
            node_id,
            socket: Arc::new(socket),
            routing_table,
            pending_queries: HashMap::new(),
            next_tid: 0,
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
            DhtNodeCommand::AddNode { id, addr } => {}
            DhtNodeCommand::GetPeers { info_hash, reply } => {}
            DhtNodeCommand::Bootstrap { addrs } => {}
            DhtNodeCommand::RoutingTableSize { reply } => {}
        }
    }

    async fn bootstrap_task(
        bootstrap_addrs: Vec<SocketAddr>,
        our_node_id: NodeId,
        dht_node_tx: mpsc::Sender<DhtNodeCommand>,
    ) {
        let tx_id = 0;
        const ALPHA: usize = 5;
        let fut = FuturesUnordered::new();
        for addr in bootstrap_addrs {
            let (find_node_tx, find_node_rx) = oneshot::channel();
            let future = async move {
                // todo: use dht_node_tx to trigger a send a find node and send the response of that
                // in this find_node_tx and finnaly we await to find_node_rx
                find_node_rx.await
            };

            fut.push(future);

            // I think we should not own the Arc Socket and tell the DhtNode to send this query, he
            // will manage the correct tx_id an correlate it with the transaction map
            // we should have  a callback to to await the response from the dht actor and move this
            // future to the fut futures unorered
            // let _ = socket.send_to(&find_node_msg.to_bytes(), addr).await;
        }

        // as we discover nodes we want to sort them based on how close they are to our own node id.

        loop {
            // Keep 5 futures running concurrently every time we can
            while fut.len() < ALPHA {
                todo!()
            }

            // await fut completion or continue loop?

            //
        }
    }

    async fn handle_packet(&mut self, addr: SocketAddr, bytes: Vec<u8>) {
        if let Ok(msg) = KrpcMessage::from_bytes(&bytes) {
            match msg.body {
                MessageBody::Query(query) => {
                    self.handle_query(addr, msg.transaction_id, query).await;
                }
                MessageBody::Response(response) => {
                    let tid = msg.transaction_id.as_u16();
                    if let Some(tx) = self.pending_queries.remove(&tid) {
                        let _ = tx.send(response);
                    }
                }
                MessageBody::Error {
                    code: _,
                    message: _,
                } => {
                    // TODO: Log or handle error
                }
            }
        }
    }

    async fn handle_query(&mut self, addr: SocketAddr, tid: TransactionId, query: Query) {
        match query {
            Query::Ping { id: _ } => {
                let msg = KrpcMessage::ping_response(tid, self.node_id);
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
}

impl DhtCoordinator {
    async fn run(mut self) {
        let mut maintenance = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await,
                        None => break,
                    }
                }
                _ = maintenance.tick() => {
                    self.maintenance().await;
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: DhtCommand) {
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

    async fn bootstrap(&self, addrs: Vec<SocketAddr>) {
        let mut v4_addrs = Vec::new();
        let mut v6_addrs = Vec::new();

        for addr in addrs {
            if addr.is_ipv4() {
                v4_addrs.push(addr);
            } else {
                v6_addrs.push(addr);
            }
        }

        // TODO:
        // Create two mechanism to listen to a reponse from each DHTnode until they yield bootstrap
        // ended
        if let Some(ref node) = self.node_v4 {
            let _ = node
                .tx
                .send(DhtNodeCommand::Bootstrap { addrs: v4_addrs })
                .await;
        }
        if let Some(ref node) = self.node_v6 {
            let _ = node
                .tx
                .send(DhtNodeCommand::Bootstrap { addrs: v6_addrs })
                .await;
        }

        // <- await until both boostraped, get_peer command otherwise cannot be processed due to
        // unhealthy routing table
    }
}

async fn resolve_bootstrap(bootstrap_nodes: Vec<BootstrapNode>) -> Vec<SocketAddr> {
    let mut futures = FuturesUnordered::new();
    for node in bootstrap_nodes {
        let host = node.host;
        let port = node.port;
        futures.push(async move {
            lookup_host(format!("{host}:{port}"))
                .await
                .map(std::iter::Iterator::collect::<Vec<_>>)
                .unwrap_or_default()
        });
    }

    let mut addrs = Vec::new();
    while let Some(mut host_addrs) = futures.next().await {
        addrs.append(&mut host_addrs);
    }
    addrs
}
