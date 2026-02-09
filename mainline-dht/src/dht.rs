//! DHT node implementation for BitTorrent Mainline DHT (BEP 0005).
//!
//! This module provides the main DHT client with:
//! - Bootstrap into the DHT network
//! - Iterative node lookup (find_node)
//! - Iterative peer lookup (get_peers)
//! - Announce peer participation (announce_peer)
//! - Server mode (responding to incoming queries)

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};

use bittorrent_common::types::InfoHash;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    time::{timeout, interval},
};

use crate::{
    error::DhtError,
    message::{CompactNodeInfo, KrpcMessage, MessageBody, Query, Response, TransactionId},
    node::Node,
    node_id::NodeId,
    peer_store::PeerStore,
    routing_table::{K, RoutingTable},
    search::{SearchManager, SearchState, CandidateStatus, MAX_INFLIGHT_QUERIES, TARGET_RESPONSES, QUERY_TIMEOUT_SECS},
    token::TokenManager,
};

/// Default bootstrap nodes for the BitTorrent DHT.
pub const DEFAULT_BOOTSTRAP_NODES: [&str; 4] = [
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "dht.libtorrent.org:25401",
    "router.utorrent.com:6881",
];

/// Default port for DHT.
const DEFAULT_PORT: u16 = 6881;

/// Timeout for individual queries.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of parallel queries in iterative lookup.
const ALPHA: usize = 3;

// ============================================================================
// Result Types
// ============================================================================

/// Result of a get_peers lookup.
#[derive(Debug, Clone)]
pub struct GetPeersResult {
    /// Peers found for the infohash.
    pub peers: Vec<SocketAddrV4>,
    /// Number of nodes that responded.
    pub nodes_contacted: usize,
    /// Closest nodes with their tokens (for subsequent announce).
    pub nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)>,
}

/// Result of an announce operation.
#[derive(Debug, Clone)]
pub struct AnnounceResult {
    /// Peers discovered during the get_peers phase.
    pub peers: Vec<SocketAddrV4>,
    /// Number of nodes that accepted our announce.
    pub announce_count: usize,
}

/// Response from DHT operations, matching TrackerResponse pattern.
#[derive(Debug, Clone)]
pub struct DhtResponse {
    /// Peers found for the infohash.
    pub peers: Vec<SocketAddr>,
}

// ============================================================================
// Public API: Dht handle
// ============================================================================

/// Handle to interact with the DHT node.
///
/// This is a lightweight handle that can be cloned and used from multiple tasks.
#[derive(Clone)]
pub struct Dht {
    command_tx: mpsc::Sender<DhtCommand>,
    node_id: Arc<std::sync::RwLock<NodeId>>,
}

impl Dht {
    /// Create a new DHT node bound to the specified port.
    ///
    /// If no port is specified, binds to port 6881 (default DHT port).
    /// The node is not bootstrapped yet - call `bootstrap()` to join the network.
    pub async fn new(port: Option<u16>) -> Result<Self, DhtError> {
        let port = port.unwrap_or(DEFAULT_PORT);
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);

        let socket = UdpSocket::bind(bind_addr).await?;
        let socket = Arc::new(socket);

        // Start with a random node ID (will be replaced with BEP 42 secure ID after bootstrap)
        let initial_id = NodeId::generate_random();
        let node_id = Arc::new(std::sync::RwLock::new(initial_id));

        let (command_tx, command_rx) = mpsc::channel(32);

        let actor = DhtActor::new(socket, initial_id, command_rx);

        // Spawn the actor
        let node_id_clone = node_id.clone();
        tokio::spawn(async move {
            if let Err(e) = actor.run(node_id_clone).await {
                tracing::error!("DHT actor error: {e}");
            }
        });

        Ok(Dht {
            command_tx,
            node_id,
        })
    }

    /// Bootstrap into the DHT network using default bootstrap nodes.
    ///
    /// This will:
    /// 1. Ping bootstrap nodes to discover our external IP
    /// 2. Generate a BEP 42 secure node ID
    /// 3. Perform iterative find_node to populate the routing table
    ///
    /// Returns our node ID after successful bootstrap.
    pub async fn bootstrap(&self) -> Result<NodeId, DhtError> {
        self.bootstrap_with_nodes(&DEFAULT_BOOTSTRAP_NODES).await
    }

    /// Bootstrap using custom bootstrap nodes.
    pub async fn bootstrap_with_nodes(&self, nodes: &[&str]) -> Result<NodeId, DhtError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.command_tx
            .send(DhtCommand::Bootstrap {
                nodes: nodes.iter().map(|s| s.to_string()).collect(),
                resp: resp_tx,
            })
            .await
            .map_err(|_| DhtError::ChannelClosed)?;

        resp_rx.await.map_err(|_| DhtError::ChannelClosed)?
    }

    /// Find the K closest nodes to a target ID.
    ///
    /// Performs an iterative lookup, querying progressively closer nodes
    /// until no closer nodes are found.
    pub async fn find_node(&self, target: NodeId) -> Result<Vec<CompactNodeInfo>, DhtError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.command_tx
            .send(DhtCommand::FindNode {
                target,
                resp: resp_tx,
            })
            .await
            .map_err(|_| DhtError::ChannelClosed)?;

        resp_rx.await.map_err(|_| DhtError::ChannelClosed)?
    }

    /// Find peers for a torrent infohash.
    ///
    /// Performs an iterative DHT lookup to discover peers from the network.
    /// Also returns tokens from responding nodes for use in subsequent announce.
    pub async fn get_peers(&self, info_hash: InfoHash) -> Result<GetPeersResult, DhtError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.command_tx
            .send(DhtCommand::GetPeers {
                info_hash,
                resp: resp_tx,
            })
            .await
            .map_err(|_| DhtError::ChannelClosed)?;

        resp_rx.await.map_err(|_| DhtError::ChannelClosed)?
    }

    /// Announce that we are participating in a torrent.
    ///
    /// Performs get_peers first to find closest nodes and obtain tokens,
    /// then announces to those nodes.
    ///
    /// Returns peers discovered during the get_peers phase and the number
    /// of nodes that accepted our announce.
    pub async fn announce_peer(
        &self,
        info_hash: InfoHash,
        port: u16,
    ) -> Result<AnnounceResult, DhtError> {
        self.announce_peer_ext(info_hash, port, false).await
    }

    /// Announce with implied_port option for NAT traversal.
    ///
    /// If `implied_port` is true, receiving nodes will use our DHT UDP port
    /// as the peer port (useful when behind NAT).
    pub async fn announce_peer_ext(
        &self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<AnnounceResult, DhtError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.command_tx
            .send(DhtCommand::Announce {
                info_hash,
                port,
                implied_port,
                resp: resp_tx,
            })
            .await
            .map_err(|_| DhtError::ChannelClosed)?;

        resp_rx.await.map_err(|_| DhtError::ChannelClosed)?
    }

    /// Get our current node ID.
    pub fn node_id(&self) -> NodeId {
        *self.node_id.read().unwrap()
    }

    /// Get the number of nodes in our routing table.
    pub async fn routing_table_size(&self) -> Result<usize, DhtError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.command_tx
            .send(DhtCommand::GetRoutingTableSize { resp: resp_tx })
            .await
            .map_err(|_| DhtError::ChannelClosed)?;

        resp_rx.await.map_err(|_| DhtError::ChannelClosed)
    }

    /// Gracefully shutdown the DHT node.
    pub async fn shutdown(&self) -> Result<(), DhtError> {
        self.command_tx
            .send(DhtCommand::Shutdown)
            .await
            .map_err(|_| DhtError::ChannelClosed)
    }

    /// Try to add a node to the routing table by first pinging it.
    ///
    /// This method pings the node at the given address. If the node responds,
    /// its contact information is added to the routing table according to
    /// the usual rules (per BEP 0005).
    pub async fn try_add_node(&self, addr: SocketAddr) -> Result<(), DhtError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.command_tx
            .send(DhtCommand::TryAddNode {
                addr,
                resp: resp_tx,
            })
            .await
            .map_err(|_| DhtError::ChannelClosed)?;

        resp_rx.await.map_err(|_| DhtError::ChannelClosed)?
    }
}

// ============================================================================
// Public API: DhtHandler (TrackerHandler-like interface)
// ============================================================================

/// Lightweight handle for DHT operations.
///
/// This follows the same pattern as `TrackerHandler` from tracker-client,
/// providing a simple interface for peer discovery and announcement.
#[derive(Clone)]
pub struct DhtHandler {
    dht: Dht,
}

impl DhtHandler {
    /// Create a new DHT handler.
    ///
    /// This creates the DHT node but does NOT bootstrap automatically.
    /// Call `bootstrap()` before using `get_peers()` or `announce()`.
    pub async fn new(port: Option<u16>) -> Result<Self, DhtError> {
        let dht = Dht::new(port).await?;
        Ok(Self { dht })
    }

    /// Bootstrap into the DHT network.
    ///
    /// Must be called before `get_peers()` or `announce()` to populate
    /// the routing table.
    pub async fn bootstrap(&self) -> Result<NodeId, DhtError> {
        self.dht.bootstrap().await
    }

    /// Bootstrap using custom bootstrap nodes.
    pub async fn bootstrap_with_nodes(&self, nodes: &[&str]) -> Result<NodeId, DhtError> {
        self.dht.bootstrap_with_nodes(nodes).await
    }

    /// Get peers for a torrent infohash.
    ///
    /// Performs an iterative DHT lookup and returns discovered peers.
    /// Returns a `DhtResponse` matching the `TrackerResponse` pattern.
    pub async fn get_peers(&self, info_hash: InfoHash) -> Result<DhtResponse, DhtError> {
        let result = self.dht.get_peers(info_hash).await?;

        Ok(DhtResponse {
            peers: result.peers.into_iter().map(SocketAddr::V4).collect(),
        })
    }

    /// Announce that we're downloading a torrent.
    ///
    /// Performs get_peers first to find closest nodes and obtain tokens,
    /// then announces to those nodes.
    ///
    /// Returns peers discovered during the get_peers phase.
    pub async fn announce(&self, info_hash: InfoHash, port: u16) -> Result<DhtResponse, DhtError> {
        self.announce_ext(info_hash, port, false).await
    }

    /// Announce with implied_port option for NAT traversal.
    ///
    /// If `implied_port` is true, receiving nodes will use our DHT UDP port
    /// as the peer port (useful when behind NAT).
    pub async fn announce_ext(
        &self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<DhtResponse, DhtError> {
        let result = self
            .dht
            .announce_peer_ext(info_hash, port, implied_port)
            .await?;

        Ok(DhtResponse {
            peers: result.peers.into_iter().map(SocketAddr::V4).collect(),
        })
    }

    /// Get our current node ID.
    pub fn node_id(&self) -> NodeId {
        self.dht.node_id()
    }

    // TODO:
    //A client using the DHT could receive by the PORT that they should attempt to ping the node on the received port and IP address of the remote peer. If a response to the ping is recieved, the node should attempt to insert the new contact information into their routing table according to the usual rules.
    pub async fn try_add_node(&self, node_addr: SocketAddr) -> Result<(), DhtError> {
        self.dht.try_add_node(node_addr).await
    }

    /// Get the number of nodes in our routing table.
    pub async fn routing_table_size(&self) -> Result<usize, DhtError> {
        self.dht.routing_table_size().await
    }

    /// Gracefully shutdown the DHT node.
    pub async fn shutdown(&self) -> Result<(), DhtError> {
        self.dht.shutdown().await
    }
}

// ============================================================================
// Internal: Commands
// ============================================================================

enum DhtCommand {
    Bootstrap {
        nodes: Vec<String>,
        resp: oneshot::Sender<Result<NodeId, DhtError>>,
    },
    FindNode {
        target: NodeId,
        resp: oneshot::Sender<Result<Vec<CompactNodeInfo>, DhtError>>,
    },
    GetPeers {
        info_hash: InfoHash,
        resp: oneshot::Sender<Result<GetPeersResult, DhtError>>,
    },
    Announce {
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
        resp: oneshot::Sender<Result<AnnounceResult, DhtError>>,
    },
    // Command with logging purposes
    GetRoutingTableSize {
        resp: oneshot::Sender<usize>,
    },
    /// Try to add a node by pinging it first, then adding to routing table if responsive.
    TryAddNode {
        addr: SocketAddr,
        resp: oneshot::Sender<Result<(), DhtError>>,
    },
    Shutdown,
}

// ============================================================================
// Internal: Pending request tracking
// ============================================================================

struct PendingRequest {
    resp_tx: oneshot::Sender<KrpcMessage>,
}

// ============================================================================
// Internal: DHT Actor
// ============================================================================

struct DhtActor {
    socket: Arc<UdpSocket>,
    node_id: NodeId,
    routing_table: RoutingTable,
    command_rx: mpsc::Receiver<DhtCommand>,
    next_tx_id: AtomicU16,
    pending: HashMap<Vec<u8>, PendingRequest>,
    /// Storage for announced peers.
    peer_store: PeerStore,
    /// Token generation and validation.
    token_manager: TokenManager,
    /// Active searches for concurrent query execution.
    search_manager: SearchManager,
}

impl DhtActor {
    fn new(
        socket: Arc<UdpSocket>,
        node_id: NodeId,
        command_rx: mpsc::Receiver<DhtCommand>,
    ) -> Self {
        Self {
            socket,
            node_id,
            routing_table: RoutingTable::new(node_id),
            command_rx,
            next_tx_id: AtomicU16::new(1),
            pending: HashMap::new(),
            peer_store: PeerStore::new(),
            token_manager: TokenManager::new(),
            search_manager: SearchManager::new(),
        }
    }

    fn next_transaction_id(&self) -> u16 {
        self.next_tx_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn run(mut self, shared_node_id: Arc<std::sync::RwLock<NodeId>>) -> Result<(), DhtError> {
        let mut buf = [0u8; 1500];

        loop {
            tokio::select! {
                // Handle incoming UDP packets
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((size, from)) => {
                            self.handle_incoming(&buf[..size], from).await;
                        }
                        Err(e) => {
                            tracing::warn!("UDP recv error: {e}");
                        }
                    }
                }

                // Handle commands from the public API
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        DhtCommand::Bootstrap { nodes, resp } => {
                            let result = self.bootstrap(&nodes).await;
                            if result.is_ok() {
                                // Update the shared node ID
                                *shared_node_id.write().unwrap() = self.node_id;
                            }
                            let _ = resp.send(result);
                        }
                        DhtCommand::FindNode { target, resp } => {
                            let result = self.iterative_find_node(target).await;
                            let _ = resp.send(result);
                        }
                        DhtCommand::GetPeers { info_hash, resp } => {
                            let result = self.iterative_get_peers(info_hash).await;
                            let _ = resp.send(result);
                        }
                        DhtCommand::Announce { info_hash, port, implied_port, resp } => {
                            let result = self.announce(info_hash, port, implied_port).await;
                            let _ = resp.send(result);
                        }
                        DhtCommand::GetRoutingTableSize { resp } => {
                            let _ = resp.send(self.routing_table.node_count());
                        }
                        DhtCommand::TryAddNode { addr, resp } => {
                            let result = self.try_add_node(addr).await;
                            let _ = resp.send(result);
                        }
                        DhtCommand::Shutdown => {
                            tracing::info!("DHT shutdown requested");
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    // ========================================================================
    // Bootstrap
    // ========================================================================

    async fn bootstrap(&mut self, bootstrap_nodes: &[String]) -> Result<NodeId, DhtError> {
        tracing::info!(
            "Starting DHT bootstrap with {} nodes",
            bootstrap_nodes.len()
        );

        // Resolve bootstrap addresses
        let mut addrs: Vec<SocketAddr> = Vec::new();
        for node in bootstrap_nodes {
            match node.to_socket_addrs() {
                Ok(resolved) => addrs.extend(resolved),
                Err(e) => tracing::warn!("Failed to resolve {node}: {e}"),
            }
        }

        if addrs.is_empty() {
            return Err(DhtError::BootstrapFailed);
        }

        // Ping bootstrap nodes to discover our external IP
        let mut external_ip: Option<Ipv4Addr> = None;
        let mut first_node: Option<(NodeId, SocketAddrV4)> = None;

        for addr in &addrs {
            let SocketAddr::V4(addr_v4) = addr else {
                continue; // Skip IPv6 for now
            };

            match self.ping(*addr_v4).await {
                Ok((msg, _)) => {
                    // Extract external IP from response
                    if let Some(sender_ip) = msg.sender_ip {
                        external_ip = Some(*sender_ip.ip());
                        // tracing::info!("Discovered external IP: {}", sender_ip.ip());
                    }

                    // Add the responding node
                    if let Some(node_id) = msg.get_node_id() {
                        first_node = Some((node_id, *addr_v4));
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("Bootstrap ping to {addr} failed: {e}");
                }
            }
        }

        // Generate BEP 42 secure node ID
        if let Some(ip) = external_ip {
            let mut secure_id = NodeId::generate_random();
            secure_id.secure_node_id(&IpAddr::V4(ip));
            self.node_id = secure_id;
            self.routing_table = RoutingTable::new(secure_id);
            tracing::info!("Generated BEP 42 secure node ID: {:?}", self.node_id);
        } else {
            tracing::warn!("Could not discover external IP, using random node ID");
        }

        // Add the first responding node
        if let Some((node_id, addr)) = first_node {
            let node = Node::new_good(node_id, addr);
            self.routing_table.try_add_node(node);
            tracing::info!("Added bootstrap node to routing table");
        }

        // Perform iterative find_node on ourselves to populate routing table
        tracing::info!("Starting iterative find_node to populate routing table...");
        match self.iterative_find_node(self.node_id).await {
            Ok(nodes) => {
                tracing::info!("Iterative find_node found {} nodes", nodes.len());
            }
            Err(e) => {
                tracing::warn!("Iterative find_node failed: {e}");
            }
        }

        let node_count = self.routing_table.node_count();
        if node_count == 0 {
            return Err(DhtError::BootstrapFailed);
        }

        tracing::info!("Bootstrap complete: {} nodes in routing table", node_count);
        Ok(self.node_id)
    }

    // ========================================================================
    // Iterative Find Node (Kademlia lookup)
    // ========================================================================

    async fn iterative_find_node(
        &mut self,
        target: NodeId,
    ) -> Result<Vec<CompactNodeInfo>, DhtError> {
        // Get initial candidates from routing table
        let initial = self.routing_table.get_closest_nodes(&target, K);

        if initial.is_empty() {
            tracing::debug!("iterative_find_node: no initial candidates in routing table");
            return Ok(Vec::new());
        }

        tracing::debug!(
            "iterative_find_node: starting with {} candidates",
            initial.len()
        );

        // Track all nodes we know about, sorted by distance
        let mut candidates: Vec<CompactNodeInfo> = initial
            .into_iter()
            .map(|n| CompactNodeInfo {
                node_id: n.node_id,
                addr: n.addr,
            })
            .collect();

        // Track which nodes we've queried
        let mut queried: HashSet<NodeId> = HashSet::new();

        // Track all nodes we've received
        let mut all_nodes: Vec<CompactNodeInfo> = candidates.clone();

        // Maximum iterations to prevent infinite loops
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 5;

        loop {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                tracing::debug!("iterative_find_node: max iterations reached");
                break;
            }

            // Sort candidates by distance to target
            candidates.sort_by(|a, b| {
                let dist_a = a.node_id ^ target;
                let dist_b = b.node_id ^ target;
                dist_a.as_bytes().cmp(&dist_b.as_bytes())
            });

            // Get unqueried candidates (up to ALPHA)
            let to_query: Vec<_> = candidates
                .iter()
                .filter(|n| !queried.contains(&n.node_id))
                .take(ALPHA)
                .cloned()
                .collect();

            if to_query.is_empty() {
                break;
            }

            // Query nodes sequentially (simpler than parallel for minimal impl)
            for node in to_query {
                queried.insert(node.node_id);

                match self.send_find_node(node.addr, target).await {
                    Ok((msg, _)) => {
                        // Mark node as good
                        self.routing_table.mark_node_good(&node.node_id);

                        // BEP 42 soft enforcement: log warning for non-compliant nodes
                        if let Some(resp_node_id) = msg.get_node_id()
                            && !resp_node_id.is_node_id_secure(IpAddr::V4(*node.addr.ip()))
                        {
                            tracing::debug!(
                                "Non-compliant node ID from {} (soft enforcement)",
                                node.addr
                            );
                        }

                        // Extract nodes from response
                        if let MessageBody::Response(Response::FindNode { nodes, .. }) = msg.body {
                            for node_info in nodes {
                                // Add to routing table
                                let new_node = Node::new(node_info.node_id, node_info.addr);
                                self.routing_table.try_add_node(new_node);

                                // Add to our candidates if not seen before
                                if !all_nodes.iter().any(|n| n.node_id == node_info.node_id) {
                                    all_nodes.push(node_info.clone());
                                    candidates.push(node_info);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("find_node to {} failed: {e}", node.addr);
                    }
                }
            }
        }

        // Return the K closest nodes we found
        all_nodes.sort_by(|a, b| {
            let dist_a = a.node_id ^ target;
            let dist_b = b.node_id ^ target;
            dist_a.as_bytes().cmp(&dist_b.as_bytes())
        });
        all_nodes.truncate(K);

        Ok(all_nodes)
    }

    // ========================================================================
    // Iterative Get Peers (Kademlia lookup for peers)
    // ========================================================================

    async fn iterative_get_peers(
        &mut self,
        info_hash: InfoHash,
    ) -> Result<GetPeersResult, DhtError> {
        let target = NodeId::from(&info_hash);

        // Get initial candidates from routing table
        let initial = self.routing_table.get_closest_nodes(&target, K);

        if initial.is_empty() {
            tracing::debug!("iterative_get_peers: no initial candidates in routing table");
            return Ok(GetPeersResult {
                peers: Vec::new(),
                nodes_contacted: 0,
                nodes_with_tokens: Vec::new(),
            });
        }

        tracing::debug!(
            "iterative_get_peers: starting with {} candidates for {}",
            initial.len(),
            info_hash
        );

        // Track all nodes we know about, sorted by distance
        let mut candidates: Vec<CompactNodeInfo> = initial
            .into_iter()
            .map(|n| CompactNodeInfo {
                node_id: n.node_id,
                addr: n.addr,
            })
            .collect();

        // Track which nodes we've queried
        let mut queried: HashSet<NodeId> = HashSet::new();

        // Collect peers from all responses
        let mut all_peers: Vec<SocketAddrV4> = Vec::new();

        // Track nodes that responded with their tokens (for subsequent announce)
        let mut nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)> = Vec::new();

        // Maximum iterations to prevent infinite loops
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 5;

        loop {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                tracing::debug!("iterative_get_peers: max iterations reached");
                break;
            }

            // Sort candidates by distance to target
            candidates.sort_by(|a, b| {
                let dist_a = a.node_id ^ target;
                let dist_b = b.node_id ^ target;
                dist_a.as_bytes().cmp(&dist_b.as_bytes())
            });

            // Get unqueried candidates (up to ALPHA)
            let to_query: Vec<_> = candidates
                .iter()
                .filter(|n| !queried.contains(&n.node_id))
                .take(ALPHA)
                .cloned()
                .collect();

            if to_query.is_empty() {
                break;
            }

            // Query nodes sequentially
            for node in to_query {
                queried.insert(node.node_id);

                match self.send_get_peers(node.addr, info_hash).await {
                    Ok((msg, _)) => {
                        // Mark node as good
                        self.routing_table.mark_node_good(&node.node_id);

                        // Extract response data
                        if let MessageBody::Response(Response::GetPeers {
                            token,
                            values,
                            nodes,
                            ..
                        }) = msg.body
                        {
                            // Store token for later announce
                            nodes_with_tokens.push((node.clone(), token));

                            // Collect peers if present
                            if let Some(peers) = values {
                                tracing::debug!("Got {} peers from {}", peers.len(), node.addr);
                                all_peers.extend(peers);
                            }

                            // Add closer nodes to candidates
                            if let Some(new_nodes) = nodes {
                                for node_info in new_nodes {
                                    // Add to routing table
                                    let new_node = Node::new(node_info.node_id, node_info.addr);
                                    self.routing_table.try_add_node(new_node);

                                    // Add to candidates if not seen before
                                    if !queried.contains(&node_info.node_id)
                                        && !candidates
                                            .iter()
                                            .any(|n| n.node_id == node_info.node_id)
                                    {
                                        candidates.push(node_info);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("get_peers to {} failed: {e}", node.addr);
                    }
                }
            }
        }

        // Sort nodes_with_tokens by distance and truncate to K
        nodes_with_tokens.sort_by(|a, b| {
            let dist_a = a.0.node_id ^ target;
            let dist_b = b.0.node_id ^ target;
            dist_a.as_bytes().cmp(&dist_b.as_bytes())
        });
        nodes_with_tokens.truncate(K);

        tracing::info!(
            "iterative_get_peers: found {} peers from {} nodes for {}",
            all_peers.len(),
            queried.len(),
            info_hash
        );

        Ok(GetPeersResult {
            peers: all_peers,
            nodes_contacted: queried.len(),
            nodes_with_tokens,
        })
    }

    // ========================================================================
    // Announce
    // ========================================================================

    async fn announce(
        &mut self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<AnnounceResult, DhtError> {
        // First, perform get_peers to find closest nodes and get tokens
        let get_peers_result = self.iterative_get_peers(info_hash).await?;

        let mut announce_count = 0;

        // Announce to the closest nodes that gave us tokens
        for (node, token) in get_peers_result.nodes_with_tokens.iter() {
            match self
                .send_announce_peer(node.addr, info_hash, port, token.clone(), implied_port)
                .await
            {
                Ok(_) => {
                    announce_count += 1;
                    tracing::debug!("Successfully announced to {}", node.addr);
                }
                Err(e) => {
                    tracing::debug!("Announce to {} failed: {e}", node.addr);
                }
            }
        }

        tracing::info!(
            "Announced to {}/{} nodes for {}",
            announce_count,
            get_peers_result.nodes_with_tokens.len(),
            info_hash
        );

        Ok(AnnounceResult {
            peers: get_peers_result.peers,
            announce_count,
        })
    }

    // ========================================================================
    // Send queries
    // ========================================================================

    async fn ping(&mut self, addr: SocketAddrV4) -> Result<(KrpcMessage, SocketAddr), DhtError> {
        let tx_id = self.next_transaction_id();
        let msg = KrpcMessage::ping_query(tx_id, self.node_id);
        self.send_and_wait(msg, SocketAddr::V4(addr)).await
    }

    async fn send_find_node(
        &mut self,
        addr: SocketAddrV4,
        target: NodeId,
    ) -> Result<(KrpcMessage, SocketAddr), DhtError> {
        let tx_id = self.next_transaction_id();
        let msg = KrpcMessage::find_node_query(tx_id, self.node_id, target);
        self.send_and_wait(msg, SocketAddr::V4(addr)).await
    }

    async fn send_get_peers(
        &mut self,
        addr: SocketAddrV4,
        info_hash: InfoHash,
    ) -> Result<(KrpcMessage, SocketAddr), DhtError> {
        let tx_id = self.next_transaction_id();
        let msg = KrpcMessage::get_peers_query(tx_id, self.node_id, info_hash);
        self.send_and_wait(msg, SocketAddr::V4(addr)).await
    }

    async fn send_announce_peer(
        &mut self,
        addr: SocketAddrV4,
        info_hash: InfoHash,
        port: u16,
        token: Vec<u8>,
        implied_port: bool,
    ) -> Result<(KrpcMessage, SocketAddr), DhtError> {
        let tx_id = self.next_transaction_id();
        let msg = KrpcMessage::announce_peer_query(
            tx_id,
            self.node_id,
            info_hash,
            port,
            token,
            implied_port,
        );
        self.send_and_wait(msg, SocketAddr::V4(addr)).await
    }

    // ========================================================================
    // Non-blocking queries for concurrent search
    // ========================================================================

    /// Send a get_peers query without waiting for a response.
    /// Returns the transaction ID for tracking.
    /// Used by the concurrent search system.
    async fn send_get_peers_query_async(
        &mut self,
        addr: SocketAddrV4,
        info_hash: InfoHash,
    ) -> Vec<u8> {
        let tx_id = self.next_transaction_id();
        let msg = KrpcMessage::get_peers_query(tx_id, self.node_id, info_hash);
        let tx_id_bytes = msg.transaction_id.0.clone();
        let bytes = msg.to_bytes();

        // Fire-and-forget UDP send
        if let Err(e) = self.socket.send_to(&bytes, SocketAddr::V4(addr)).await {
            tracing::warn!("Failed to send get_peers to {}: {}", addr, e);
        }

        tx_id_bytes
    }

    /// Advance a search by sending queries to pending candidates.
    /// Fills available slots up to MAX_INFLIGHT_QUERIES.
    async fn advance_search(&mut self, info_hash: InfoHash) {
        // Get the search and check available slots
        let Some(search) = self.search_manager.searches.get(&info_hash) else {
            return;
        };

        let available_slots = MAX_INFLIGHT_QUERIES.saturating_sub(search.in_flight);
        if available_slots == 0 {
            return;
        }

        // Get pending candidates to query
        let to_query: Vec<_> = search
            .get_pending_candidates(available_slots)
            .iter()
            .map(|c| (c.node.node_id, c.node.addr))
            .collect();

        if to_query.is_empty() {
            return;
        }

        // Send queries to each candidate
        for (node_id, addr) in to_query {
            let tx_id = self.send_get_peers_query_async(addr, info_hash).await;

            // Register the transaction
            self.search_manager.register_tx(tx_id.clone(), info_hash);

            // Update candidate status
            if let Some(search) = self.search_manager.searches.get_mut(&info_hash) {
                search.mark_querying(&node_id, tx_id);
                search.in_flight += 1;
            }
        }
    }

    /// Check for timed out queries in all active searches.
    async fn check_search_timeouts(&mut self) {
        let timeout_duration = Duration::from_secs(QUERY_TIMEOUT_SECS);
        let now = Instant::now();

        // Collect timed out transactions
        let mut timed_out: Vec<(InfoHash, Vec<u8>)> = Vec::new();

        for (info_hash, search) in &self.search_manager.searches {
            for candidate in &search.candidates {
                if let CandidateStatus::Querying(tx_id) = &candidate.status {
                    if let Some(sent_at) = candidate.query_sent_at {
                        if now.duration_since(sent_at) > timeout_duration {
                            timed_out.push((*info_hash, tx_id.clone()));
                        }
                    }
                }
            }
        }

        // Process timeouts
        for (info_hash, tx_id) in timed_out {
            // Remove TX mapping
            self.search_manager.remove_tx(&tx_id);

            // Mark candidate as failed
            if let Some(search) = self.search_manager.searches.get_mut(&info_hash) {
                if search.mark_failed(&tx_id).is_some() {
                    search.in_flight = search.in_flight.saturating_sub(1);
                    tracing::debug!("Query timed out for search {}", info_hash);
                }
            }
        }

        // Advance searches and check for completion
        let active_searches: Vec<InfoHash> = self.search_manager.active_searches();
        for info_hash in active_searches {
            // First advance the search to fill slots
            self.advance_search(info_hash).await;

            // Then check if it should complete
            let should_complete = self
                .search_manager
                .searches
                .get(&info_hash)
                .map(|s| s.should_complete())
                .unwrap_or(false);

            if should_complete {
                self.complete_search(info_hash).await;
            }
        }
    }

    /// Complete a search and send the result.
    async fn complete_search(&mut self, info_hash: InfoHash) {
        if let Some(search) = self.search_manager.remove_search(&info_hash) {
            let elapsed = search.started_at.elapsed();
            let peer_count = search.peers_found.len();
            let node_count = search.responded;

            tracing::info!(
                "Search completed for {}: {} peers from {} nodes in {:?}",
                info_hash,
                peer_count,
                node_count,
                elapsed
            );

            let (result, completion_tx) = search.into_result();

            // Send result if channel is still open
            if let Some(tx) = completion_tx {
                let _ = tx.send(result);
            }
        }
    }

    /// Handle a get_peers response for an active concurrent search.
    async fn handle_search_response(&mut self, msg: KrpcMessage, from: SocketAddr) {
        let tx_id = msg.transaction_id.0.clone();

        // Look up which search this belongs to
        let Some(info_hash) = self.search_manager.remove_tx(&tx_id) else {
            // Not a search response, ignore
            return;
        };

        // Get the search
        let Some(search) = self.search_manager.searches.get_mut(&info_hash) else {
            return;
        };

        // Mark candidate as responded and decrement in_flight
        if search.mark_responded(&tx_id).is_some() {
            search.in_flight = search.in_flight.saturating_sub(1);
            search.responded += 1;
        }

        // Process response data
        if let MessageBody::Response(Response::GetPeers {
            token,
            values,
            nodes,
            ..
        }) = msg.body
        {
            // Store token for announce
            let SocketAddr::V4(from_v4) = from else {
                return;
            };

            // Find the candidate that responded and add to nodes_with_tokens
            if let Some(candidate) = search.candidates.iter().find(|c| {
                matches!(&c.status, CandidateStatus::Responded) && c.node.addr == from_v4
            }) {
                search
                    .nodes_with_tokens
                    .push((candidate.node.clone(), token));
            }

            // Collect peers (deduplicated via HashSet)
            if let Some(peers) = values {
                for peer in peers {
                    search.peers_found.insert(peer);
                }
                tracing::debug!(
                    "Got {} peers from {} for {}",
                    search.peers_found.len(),
                    from,
                    info_hash
                );
            }

            // Add closer nodes to candidates
            if let Some(new_nodes) = nodes {
                // Add to routing table
                for node_info in &new_nodes {
                    let new_node = Node::new(node_info.node_id, node_info.addr);
                    self.routing_table.try_add_node(new_node);
                }

                // Add to search candidates
                if let Some(search) = self.search_manager.searches.get_mut(&info_hash) {
                    search.add_candidates(new_nodes);
                }
            }
        }

        // Check if search should complete
        let should_complete = self
            .search_manager
            .searches
            .get(&info_hash)
            .map(|s| s.should_complete())
            .unwrap_or(false);

        if should_complete {
            self.complete_search(info_hash).await;
        } else {
            // Continue search - fill available slots
            self.advance_search(info_hash).await;
        }
    }

    async fn send_and_wait(
        &mut self,
        msg: KrpcMessage,
        addr: SocketAddr,
    ) -> Result<(KrpcMessage, SocketAddr), DhtError> {
        // Why not use directly u16?
        let tx_id = msg.transaction_id.0.clone();
        let bytes = msg.to_bytes();

        // Create response channel
        let (resp_tx, mut resp_rx) = oneshot::channel();
        self.pending
            .insert(tx_id.clone(), PendingRequest { resp_tx });

        // Send the message
        self.socket.send_to(&bytes, addr).await?;

        // Wait for response with timeout, polling for incoming messages
        let mut buf = [0u8; 1500];
        let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                self.pending.remove(&tx_id);
                return Err(DhtError::Timeout);
            }

            tokio::select! {
                result = &mut resp_rx => {
                    self.pending.remove(&tx_id);
                    match result {
                        Ok(response) => return Ok((response, addr)),
                        Err(_) => return Err(DhtError::ChannelClosed),
                    }
                }
                result = timeout(remaining, self.socket.recv_from(&mut buf)) => {
                    match result {
                        Ok(Ok((size, from))) => {
                            self.handle_incoming(&buf[..size], from).await;
                        }
                        Ok(Err(e)) => {
                            self.pending.remove(&tx_id);
                            return Err(DhtError::Network(e));
                        }
                        Err(_) => {
                            // Timeout on recv, continue loop to check overall deadline
                        }
                    }
                }
            }
        }
    }

    // ========================================================================
    // Handle incoming messages
    // ========================================================================

    async fn handle_incoming(&mut self, data: &[u8], from: SocketAddr) {
        let msg = match KrpcMessage::from_bytes(data) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("Failed to parse message from {from}: {e}");
                return;
            }
        };

        match &msg.body {
            MessageBody::Query(query) => {
                tracing::debug!("Received query from {from}");
                self.handle_query(&msg, query, from).await;
            }
            MessageBody::Response(_) => {
                // Correlate with pending request
                let tx_id = msg.transaction_id.0.clone();
                tracing::debug!(
                    "Received response from {from}, tx_id={:?}, pending_count={}",
                    tx_id,
                    self.pending.len()
                );

                // Check if this is a response for an active concurrent search
                if self.search_manager.lookup_tx(&tx_id).is_some() {
                    self.handle_search_response(msg, from).await;
                } else if let Some(pending) = self.pending.remove(&tx_id) {
                    // Legacy path: send to pending channel (used by send_and_wait)
                    let _ = pending.resp_tx.send(msg);
                } else {
                    tracing::debug!("Received response for unknown transaction from {from}");
                }
            }
            MessageBody::Error { code, message } => {
                tracing::debug!("Received error from {from}: [{code}] {message}");
            }
        }
    }

    async fn handle_query(&mut self, msg: &KrpcMessage, query: &Query, from: SocketAddr) {
        let response = match query {
            Query::Ping { .. } => {
                KrpcMessage::ping_response(msg.transaction_id.clone(), self.node_id)
            }
            Query::FindNode { target, .. } => {
                let closest = self.routing_table.get_closest_nodes(target, K);
                let nodes: Vec<CompactNodeInfo> = closest
                    .into_iter()
                    .map(|n| CompactNodeInfo {
                        node_id: n.node_id,
                        addr: n.addr,
                    })
                    .collect();
                KrpcMessage::find_node_response(msg.transaction_id.clone(), self.node_id, nodes)
            }
            Query::GetPeers { info_hash, .. } => {
                self.handle_get_peers_query(msg.transaction_id.clone(), from, info_hash)
            }
            Query::AnnouncePeer {
                info_hash,
                port,
                token,
                implied_port,
                ..
            } => self.handle_announce_peer_query(
                msg.transaction_id.clone(),
                from,
                *info_hash,
                *port,
                token,
                *implied_port,
            ),
        };

        let bytes = response.to_bytes();
        if let Err(e) = self.socket.send_to(&bytes, from).await {
            tracing::warn!("Failed to send response to {from}: {e}");
        }
    }

    /// Handle a get_peers query from another node.
    fn handle_get_peers_query(
        &mut self,
        tx_id: TransactionId,
        from: SocketAddr,
        info_hash: &InfoHash,
    ) -> KrpcMessage {
        let SocketAddr::V4(from_v4) = from else {
            return KrpcMessage::error_response(
                tx_id,
                crate::message::error_codes::SERVER_ERROR,
                "IPv6 not supported".to_string(),
            );
        };

        // Generate token for this IP
        let token = self.token_manager.generate(from_v4.ip());

        // Check if we have peers for this infohash
        let peers = self.peer_store.get_peers(info_hash);

        if !peers.is_empty() {
            tracing::debug!("Returning {} peers for {}", peers.len(), info_hash);
            KrpcMessage::get_peers_response_with_values(tx_id, self.node_id, token, peers)
        } else {
            // Return closest nodes
            let target = NodeId::from(info_hash);
            let closest = self.routing_table.get_closest_nodes(&target, K);
            let nodes: Vec<CompactNodeInfo> = closest
                .into_iter()
                .map(|n| CompactNodeInfo {
                    node_id: n.node_id,
                    addr: n.addr,
                })
                .collect();
            tracing::debug!(
                "No peers for {}, returning {} closest nodes",
                info_hash,
                nodes.len()
            );
            KrpcMessage::get_peers_response_with_nodes(tx_id, self.node_id, token, nodes)
        }
    }

    /// Handle an announce_peer query from another node.
    fn handle_announce_peer_query(
        &mut self,
        tx_id: TransactionId,
        from: SocketAddr,
        info_hash: InfoHash,
        port: u16,
        token: &[u8],
        implied_port: bool,
    ) -> KrpcMessage {
        let SocketAddr::V4(from_v4) = from else {
            return KrpcMessage::error_response(
                tx_id,
                crate::message::error_codes::SERVER_ERROR,
                "IPv6 not supported".to_string(),
            );
        };

        // Validate token
        if !self.token_manager.validate(from_v4.ip(), token) {
            tracing::debug!("Invalid token from {} for announce_peer", from);
            return KrpcMessage::error_response(
                tx_id,
                crate::message::error_codes::PROTOCOL_ERROR,
                "bad token".to_string(),
            );
        }

        // Determine peer address (implied_port uses UDP source port)
        let peer_port = if implied_port { from_v4.port() } else { port };
        let peer_addr = SocketAddrV4::new(*from_v4.ip(), peer_port);

        // Store the peer
        self.peer_store.add_peer(info_hash, peer_addr);
        tracing::debug!("Stored peer {} for {}", peer_addr, info_hash);

        KrpcMessage::announce_peer_response(tx_id, self.node_id)
    }

    /// Try to add a node to the routing table by pinging it first.
    ///
    /// Per BEP 0005: Peers that receive a PORT message should attempt to ping
    /// the node on the received port and IP address. If a response is received,
    /// the node should be inserted into the routing table according to the usual rules.
    async fn try_add_node(&mut self, addr: SocketAddr) -> Result<(), DhtError> {
        let SocketAddr::V4(addr_v4) = addr else {
            return Err(DhtError::Network(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "IPv6 not supported",
            )));
        };

        tracing::debug!("Attempting to add node {} to routing table", addr);

        // Ping the node to verify it's responsive
        match self.ping(addr_v4).await {
            Ok((msg, _)) => {
                // Extract the node ID from the response
                if let Some(node_id) = msg.get_node_id() {
                    // Create a new node marked as good (since it responded)
                    let node = Node::new_good(node_id, addr_v4);

                    // Add to routing table according to usual rules
                    if self.routing_table.try_add_node(node) {
                        tracing::info!("Added node {} ({:?}) to routing table", addr, node_id);
                        Ok(())
                    } else {
                        tracing::debug!(
                            "Node {} ({:?}) not added (bucket full or same as us)",
                            addr,
                            node_id
                        );
                        Ok(())
                    }
                } else {
                    tracing::warn!("Ping response from {} missing node ID", addr);
                    Err(DhtError::InvalidResponse(
                        "Ping response missing node ID".to_string(),
                    ))
                }
            }
            Err(e) => {
                tracing::debug!("Failed to ping node at {}: {}", addr, e);
                Err(e)
            }
        }
    }
}
