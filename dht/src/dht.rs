use crate::error::DhtError;
use bytes::{Bytes, BytesMut};
use futures::{StreamExt, stream::FuturesUnordered};
use std::{
    net::{Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
};
use tokio::{
    net::{UdpSocket, lookup_host},
    sync::{mpsc, oneshot},
};

/// Default port for DHT.
const DEFAULT_PORT: u16 = 6881;

// BEP-32 requires max 1024-byte payloads:
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

/// A DHT bootstrap node (host + port, unresolved).
/// Kept as strings because DNS resolution happens at connection time.
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
        info_hash: [u8; 20],
        peer_port: u16,
        tx: oneshot::Sender<Vec<SocketAddr>>,
    },
    FindPeer {
        info_hash: [u8; 20],
        tx: oneshot::Sender<Vec<SocketAddr>>,
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
        info_hash: [u8; 20],
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
            .map_err(op)?;
        rx.await.map_err(op)
    }

    pub async fn find_peers(&self, info_hash: [u8; 20]) -> Result<Vec<SocketAddr>, DhtError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DhtCommand::FindPeer { info_hash, tx })
            .await
            .map_err(op)?;
        rx.await.map_err(op)
    }
}

struct DhtActor {
    rx: mpsc::Receiver<DhtCommand>,
    node_id: NodeId,
    dht_v4: DhtV4,
    dht_v6: Option<DhtV6>,
}

// The two DHTs are independent, meaning that no IPv6 data is stored in the IPv4 DHT, and, conversely, no IPv4 data is stored in the IPv6 DHT. A node wishing to participate in both DHTs must maintain two distinct routing tables, one for IPv4 and one for IPv6.
struct DhtV4 {
    bucket: Vec<Bucket<SocketAddrV4>>,
    pub socket: Arc<UdpSocket>,
}

impl DhtV4 {
    pub async fn new(port: u16) -> Result<Self, DhtError> {
        let s = UdpSocket::bind(format!("0.0.0.0:{port}")).await?;

        Ok(Self {
            socket: s.into(),
            bucket: Vec::with_capacity(128),
        })
    }
}

struct DhtV6 {
    socket: Arc<UdpSocket>,
    bucket: Vec<Bucket<SocketAddrV6>>,
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

impl DhtV6 {
    pub fn new(port: u16) -> Result<Self, DhtError> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        socket.set_only_v6(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), port).into())?;
        let socket = UdpSocket::from_std(socket.into())?;
        Ok(Self {
            socket: socket.into(),
            bucket: Vec::with_capacity(128),
        })
    }
}

impl DhtActor {
    fn get_nodes_from_bootstrap_file() {}

    pub async fn new(cfg: &DhtConfig, rx: mpsc::Receiver<DhtCommand>) -> Result<Self, DhtError> {
        let node_id = NodeId([0u8; 20]);
        let dht_v4 = DhtV4::new(cfg.port).await?;
        let dht_v6 = DhtV6::new(cfg.port)
            .inspect_err(|e| tracing::warn!("v6 DHT unavailable: {e}"))
            .ok();

        Ok(Self {
            rx,
            node_id,
            dht_v4,
            dht_v6,
        })
    }

    fn send_message(&self, addr: SocketAddr, message_payload: &Bytes) {
        let socket = if let Some(v6) = &self.dht_v6
            && addr.is_ipv6()
        {
            v6.socket.clone()
        } else {
            self.dht_v4.socket.clone()
        };

        let _ = socket.try_send_to(message_payload, addr);
    }

    fn build_ping_message(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(512);
        buf.extend_from_slice(b"d1:ad2:id20:");
        buf.extend_from_slice(&self.node_id.0);

        // rc = snprintf(buf + i, 512 - i, "e1:q4:ping1:t%d:", tid_len);
        let tid = todo!();
        buf.extend_from_slice(tid);

        buf.split().freeze()
    }

    fn ping_node(&self, addr: SocketAddr) {
        let ping_message: Bytes = self.build_ping_message();
        self.send_message(addr, &ping_message);
    }

    pub async fn run(mut self, bootstrap_nodes: Vec<BootstrapNode>) {
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

        let (packet_tx, mut packet_rx) = mpsc::channel::<(SocketAddr, Bytes)>(128);

        let socket = self.dht_v4.socket.clone();
        let tx = packet_tx.clone();
        tokio::spawn(async move {
            let _ = run_socket(socket, tx).await;
        });

        if let Some(v6) = &self.dht_v6 {
            let socket = v6.socket.clone();
            let tx = packet_tx.clone();
            tokio::spawn(async move {
                let _ = run_socket(socket, tx).await;
            });
        }

        loop {
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => todo!(),
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

    async fn handle_incoming(&mut self, (addr, bytes): (SocketAddr, Bytes)) {
        let Ok(message) = DhtMessage::from_bytes(bytes) else {
            return;
        };
    }
}

#[derive(Debug, Clone)]
enum DhtMessage {
    /// Query: ping
    Ping { id: NodeId },
    /// Response: pong  
    Pong { id: NodeId },
    /// Query: find nodes
    FindNode {
        id: NodeId,
        target: NodeId,
        want: Want,
    },
    /// Response: these are closest nodes
    Nodes {
        id: NodeId,
        nodes: Vec<u8>,
        token: Option<Vec<u8>>,
    },
    /// Query: get peers for this info hash
    GetPeers {
        id: NodeId,
        info_hash: [u8; 20],
        want: Want,
    },
    /// Response: here are peers
    Values {
        id: NodeId,
        token: Option<Vec<u8>>,
        values: Vec<SocketAddr>,
    },
    /// Query: announce peer
    AnnouncePeer {
        id: NodeId,
        info_hash: [u8; 20],
        port: u16,
        token: Vec<u8>,
    },
    /// Response: acknowledged
    PeerAnnounced { id: NodeId },
    /// Error response
    Error { message: String },
}

#[derive(Clone, Copy, Debug)]
struct Want {}

impl DhtMessage {
    pub fn from_bytes(bytes: Bytes) -> Result<Self, DhtError> {
        todo!()
    }
}

pub const NODE_ID_LEN: usize = 20;

#[derive(Debug, Copy, Clone)]
struct NodeId([u8; NODE_ID_LEN]);
#[derive(Debug)]
pub struct Node<A> {
    pub id: NodeId,
    pub addr: A,
}

#[derive(Debug)]
pub struct Bucket<A> {
    pub nodes: Vec<Node<A>>,
    pub cache: Vec<Node<A>>,
}
