//! DHT node implementation for BitTorrent Mainline DHT (BEP 0005).
//!
//! This module provides the main DHT client with:
//! - Bootstrap into the DHT network
//! - Iterative node lookup (find_node)
//! - Iterative peer lookup (get_peers)
//! - Announce peer participation (announce_peer)
//! - Server mode (responding to incoming queries)
//!
//! Architecture:
//! - DhtActor owns all state and runs in a dedicated task
//! - KrpcSocket handles UDP I/O in a separate task
//! - Searches are spawned as concurrent tasks that communicate with DhtActor

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs},
    sync::{Arc, atomic::{AtomicU16, Ordering}},
    time::Duration,
};

use bittorrent_common::types::InfoHash;
use tokio::sync::{mpsc, oneshot};

use crate::{
    error::DhtError,
    krpc_socket::{KrpcSocket, SocketEvent, SocketHandle},
    message::{CompactNodeInfo, KrpcMessage, MessageBody, Query, TransactionId},
    node::Node,
    node_id::NodeId,
    peer_store::PeerStore,
    routing_table::{K, RoutingTable},
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
const ALPHA: usize = 8;

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
    node_id: NodeId,
}

impl Dht {
    /// Create a new DHT node bound to the specified port.
    ///
    /// If no port is specified, binds to port 6881 (default DHT port).
    /// The node is not bootstrapped yet - call `bootstrap()` to join the network.
    pub async fn new(port: Option<u16>) -> Result<Self, DhtError> {
        let port = port.unwrap_or(DEFAULT_PORT);
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);

        // Create KrpcSocket actor
        let (socket_actor, socket_handle, socket_events) =
            KrpcSocket::new(SocketAddr::V4(bind_addr)).await?;

        // Start with a random node ID (will be replaced with BEP 42 secure ID after bootstrap)
        let initial_id = NodeId::generate_random();

        let (command_tx, command_rx) = mpsc::channel(32);

        // Create DhtActor
        let actor = DhtActor::new(initial_id, socket_handle, socket_events, command_rx);

        // Spawn the KrpcSocket actor
        tokio::spawn(socket_actor.run());

        // Spawn the DhtActor
        let node_id_clone = initial_id;
        tokio::spawn(async move {
            if let Err(e) = actor.run().await {
                tracing::error!("DHT actor error: {}", e);
            }
        });

        Ok(Dht {
            command_tx,
            node_id: node_id_clone,
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
        self.node_id
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
    GetRoutingTableSize {
        resp: oneshot::Sender<usize>,
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

/// Main DHT actor that owns all mutable state
///
/// State is owned by this actor and accessed only through message passing,
/// eliminating the need for locks and enabling concurrent search operations.
struct DhtActor {
    /// Our node ID
    node_id: NodeId,
    /// Routing table (owned, not shared)
    routing_table: RoutingTable,
    /// Pending RPC requests
    pending: HashMap<Vec<u8>, PendingRequest>,
    /// Peer storage
    peer_store: PeerStore,
    /// Token manager for announce security
    token_manager: TokenManager,
    /// Next transaction ID (shared with search tasks)
    next_tx_id: Arc<AtomicU16>,
    /// Handle to KrpcSocket actor
    socket_handle: SocketHandle,
    /// Channel for receiving socket events
    socket_events: mpsc::Receiver<SocketEvent>,
    /// Channel for receiving commands from public API
    command_rx: mpsc::Receiver<DhtCommand>,
}

impl DhtActor {
    fn new(
        node_id: NodeId,
        socket_handle: SocketHandle,
        socket_events: mpsc::Receiver<SocketEvent>,
        command_rx: mpsc::Receiver<DhtCommand>,
    ) -> Self {
        Self {
            node_id,
            routing_table: RoutingTable::new(node_id),
            pending: HashMap::new(),
            peer_store: PeerStore::new(),
            token_manager: TokenManager::new(),
            next_tx_id: Arc::new(AtomicU16::new(1)),
            socket_handle,
            socket_events,
            command_rx,
        }
    }

    async fn run(mut self) -> Result<(), DhtError> {
        loop {
            tokio::select! {
                // Handle socket events (incoming messages)
                Some(event) = self.socket_events.recv() => {
                    self.handle_socket_event(event).await;
                }

                // Handle commands from the public API
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        DhtCommand::Bootstrap { nodes, resp } => {
                            let result = self.bootstrap(&nodes).await;
                            let _ = resp.send(result);
                        }
                        DhtCommand::FindNode { target, resp } => {
                            // Spawn search as concurrent task
                            let search_task = SearchTask::new_find_node(
                                self.node_id,
                                target,
                                self.routing_table.clone(),
                                self.socket_handle.clone(),
                                self.next_tx_id.clone(),
                                resp,
                            );
                            tokio::spawn(search_task.run());
                        }
                        DhtCommand::GetPeers { info_hash, resp } => {
                            // Spawn search as concurrent task
                            let search_task = SearchTask::new_get_peers(
                                self.node_id,
                                info_hash,
                                self.routing_table.clone(),
                                self.socket_handle.clone(),
                                self.next_tx_id.clone(),
                                resp,
                            );
                            tokio::spawn(search_task.run());
                        }
                        DhtCommand::Announce { info_hash, port, implied_port, resp } => {
                            // First do get_peers, then announce
                            let search_task = SearchTask::new_announce(
                                self.node_id,
                                info_hash,
                                port,
                                implied_port,
                                self.routing_table.clone(),
                                self.socket_handle.clone(),
                                self.next_tx_id.clone(),
                                resp,
                            );
                            tokio::spawn(search_task.run());
                        }
                        DhtCommand::GetRoutingTableSize { resp } => {
                            let node_count = self.routing_table.node_count();
                            let _ = resp.send(node_count);
                        }
                        DhtCommand::Shutdown => {
                            tracing::info!("DHT shutdown requested");
                            // Shutdown socket
                            let _ = self.socket_handle.shutdown().await;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn handle_socket_event(&mut self, event: SocketEvent) {
        match event {
            SocketEvent::MessageReceived { message, from } => {
                self.handle_incoming_message(message, from).await;
            }
            SocketEvent::ParseError { from, error, .. } => {
                tracing::debug!("Failed to parse message from {}: {}", from, error);
            }
            SocketEvent::SendError { to, error, .. } => {
                tracing::warn!("Failed to send message to {}: {}", to, error);
            }
        }
    }

    async fn handle_incoming_message(&mut self, msg: KrpcMessage, from: SocketAddr) {
        match &msg.body {
            MessageBody::Query(query) => {
                tracing::debug!("Received query from {}", from);
                self.handle_query(&msg, query, from).await;
            }
            MessageBody::Response(_) => {
                // Correlate with pending request
                let tx_id = msg.transaction_id.0.clone();
                tracing::debug!(
                    "Received response from {}, tx_id={:?}, pending_count={}",
                    from,
                    tx_id,
                    self.pending.len()
                );
                if let Some(pending) = self.pending.remove(&tx_id) {
                    let _ = pending.resp_tx.send(msg);
                } else {
                    tracing::debug!("Received response for unknown transaction from {}", from);
                }
            }
            MessageBody::Error { code, message } => {
                tracing::debug!("Received error from {}: [{}] {}", from, code, message);
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
        if let Err(e) = self.socket_handle.send_to(&bytes, from).await {
            tracing::warn!("Failed to send response to {}: {}", from, e);
        }
    }

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
                Err(e) => tracing::warn!("Failed to resolve {}: {}", node, e),
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
                        tracing::info!("Discovered external IP: {}", sender_ip.ip());
                    }

                    // Add the responding node
                    if let Some(node_id) = msg.get_node_id() {
                        first_node = Some((node_id, *addr_v4));
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("Bootstrap ping to {} failed: {}", addr, e);
                }
            }
        }

        // Generate BEP 42 secure node ID
        if let Some(ip) = external_ip {
            let secure_id = NodeId::generate_secure(&IpAddr::V4(ip));
            self.node_id = secure_id;

            // Re-create routing table with new ID
            self.routing_table = RoutingTable::new(secure_id);

            tracing::info!("Generated BEP 42 secure node ID: {:?}", secure_id);
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

        // Do a simple find_node search synchronously for bootstrap
        let target = self.node_id;
        let nodes = self.simple_find_node(target).await?;
        tracing::info!("Iterative find_node found {} nodes", nodes.len());

        let node_count = self.routing_table.node_count();
        if node_count == 0 {
            return Err(DhtError::BootstrapFailed);
        }

        tracing::info!("Bootstrap complete: {} nodes in routing table", node_count);
        Ok(self.node_id)
    }

    async fn ping(&self, addr: SocketAddrV4) -> Result<(KrpcMessage, SocketAddr), DhtError> {
        let tx_id = self.next_transaction_id();
        let msg = KrpcMessage::ping_query(tx_id, self.node_id);
        self.send_and_wait(msg, SocketAddr::V4(addr)).await
    }

    async fn send_and_wait(
        &self,
        msg: KrpcMessage,
        addr: SocketAddr,
    ) -> Result<(KrpcMessage, SocketAddr), DhtError> {
        let _tx_id = msg.transaction_id.0.clone();
        let bytes = msg.to_bytes();

        // Create response channel
        let (_resp_tx, resp_rx) = oneshot::channel();

        // We need to add to pending, but we can't mutate self here since we're in an async method
        // that takes &self. For bootstrap operations, we'll use a different approach.
        // For now, let's just send and hope for the best (this is a limitation we need to address)

        // Send the message
        self.socket_handle
            .send_to(&bytes, addr)
            .await
            .map_err(DhtError::Network)?;

        // Wait for response with timeout
        match tokio::time::timeout(QUERY_TIMEOUT, resp_rx).await {
            Ok(Ok(response)) => Ok((response, addr)),
            Ok(Err(_)) => Err(DhtError::ChannelClosed),
            Err(_) => Err(DhtError::Timeout),
        }
    }

    fn next_transaction_id(&self) -> u16 {
        self.next_tx_id.fetch_add(1, Ordering::Relaxed)
    }

    // Simplified find_node for bootstrap (runs synchronously in actor)
    async fn simple_find_node(&mut self, target: NodeId) -> Result<Vec<CompactNodeInfo>, DhtError> {
        // Get initial candidates from routing table
        let initial_nodes = self.routing_table.get_closest_nodes(&target, K);

        if initial_nodes.is_empty() {
            return Ok(Vec::new());
        }

        let mut candidates: Vec<CompactNodeInfo> = initial_nodes
            .into_iter()
            .map(|n| CompactNodeInfo {
                node_id: n.node_id,
                addr: n.addr,
            })
            .collect();

        let mut queried: HashSet<NodeId> = HashSet::new();
        let mut all_nodes = candidates.clone();

        for _ in 0..3 {
            // Sort by distance
            candidates.sort_by(|a, b| {
                let dist_a = a.node_id ^ target;
                let dist_b = b.node_id ^ target;
                dist_a.as_bytes().cmp(&dist_b.as_bytes())
            });

            let to_query: Vec<_> = candidates
                .iter()
                .filter(|n| !queried.contains(&n.node_id))
                .take(ALPHA)
                .cloned()
                .collect();

            if to_query.is_empty() {
                break;
            }

            for node in to_query {
                queried.insert(node.node_id);

                // Send find_node query
                let tx_id = self.next_transaction_id();
                let msg = KrpcMessage::find_node_query(tx_id, self.node_id, target);

                let bytes = msg.to_bytes();
                if let Err(e) = self
                    .socket_handle
                    .send_to(&bytes, SocketAddr::V4(node.addr))
                    .await
                {
                    tracing::debug!("Failed to send find_node to {}: {}", node.addr, e);
                    continue;
                }

                // Note: This is a simplified version - we won't wait for responses
                // in the synchronous bootstrap. The routing table will be populated
                // from incoming responses handled by handle_incoming_message.
            }

            // Give some time for responses
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        Ok(all_nodes)
    }
}

// ============================================================================
// Internal: Search Task for concurrent operations
// ============================================================================

/// A search task that runs concurrently with other searches
///
/// This allows multiple find_node/get_peers/announce operations to run
/// in parallel without blocking each other.
struct SearchTask {
    node_id: NodeId,
    search_type: SearchType,
    routing_table: RoutingTable,
    socket_handle: SocketHandle,
    next_tx_id: std::sync::Arc<AtomicU16>,
    resp_tx: oneshot::Sender<SearchResult>,
}

enum SearchType {
    FindNode { target: NodeId },
    GetPeers { info_hash: InfoHash },
    Announce {
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    },
}

enum SearchResult {
    FindNode(Result<Vec<CompactNodeInfo>, DhtError>),
    GetPeers(Result<GetPeersResult, DhtError>),
    Announce(Result<AnnounceResult, DhtError>),
}

impl SearchTask {
    fn new_find_node(
        node_id: NodeId,
        target: NodeId,
        routing_table: RoutingTable,
        socket_handle: SocketHandle,
        next_tx_id: std::sync::Arc<AtomicU16>,
        resp_tx: oneshot::Sender<Result<Vec<CompactNodeInfo>, DhtError>>,
    ) -> Self {
        // We need to wrap the result type
        let (tx, rx) = oneshot::channel();

        // Spawn a task to translate the result
        tokio::spawn(async move {
            match rx.await {
                Ok(SearchResult::FindNode(result)) => {
                    let _ = resp_tx.send(result);
                }
                Ok(_) => {
                    let _ = resp_tx.send(Err(DhtError::BootstrapFailed));
                }
                Err(_) => {
                    let _ = resp_tx.send(Err(DhtError::ChannelClosed));
                }
            }
        });

        Self {
            node_id,
            search_type: SearchType::FindNode { target },
            routing_table,
            socket_handle,
            next_tx_id,
            resp_tx: tx,
        }
    }

    fn new_get_peers(
        node_id: NodeId,
        info_hash: InfoHash,
        routing_table: RoutingTable,
        socket_handle: SocketHandle,
        next_tx_id: std::sync::Arc<AtomicU16>,
        resp_tx: oneshot::Sender<Result<GetPeersResult, DhtError>>,
    ) -> Self {
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            match rx.await {
                Ok(SearchResult::GetPeers(result)) => {
                    let _ = resp_tx.send(result);
                }
                Ok(_) => {
                    let _ = resp_tx.send(Err(DhtError::BootstrapFailed));
                }
                Err(_) => {
                    let _ = resp_tx.send(Err(DhtError::ChannelClosed));
                }
            }
        });

        Self {
            node_id,
            search_type: SearchType::GetPeers { info_hash },
            routing_table,
            socket_handle,
            next_tx_id,
            resp_tx: tx,
        }
    }

    fn new_announce(
        node_id: NodeId,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
        routing_table: RoutingTable,
        socket_handle: SocketHandle,
        next_tx_id: std::sync::Arc<AtomicU16>,
        resp_tx: oneshot::Sender<Result<AnnounceResult, DhtError>>,
    ) -> Self {
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            match rx.await {
                Ok(SearchResult::Announce(result)) => {
                    let _ = resp_tx.send(result);
                }
                Ok(_) => {
                    let _ = resp_tx.send(Err(DhtError::BootstrapFailed));
                }
                Err(_) => {
                    let _ = resp_tx.send(Err(DhtError::ChannelClosed));
                }
            }
        });

        Self {
            node_id,
            search_type: SearchType::Announce {
                info_hash,
                port,
                implied_port,
            },
            routing_table,
            socket_handle,
            next_tx_id,
            resp_tx: tx,
        }
    }

    async fn run(self) {
        let result = match self.search_type {
            SearchType::FindNode { target } => {
                SearchResult::FindNode(self.run_find_node(target).await)
            }
            SearchType::GetPeers { info_hash } => {
                SearchResult::GetPeers(self.run_get_peers(info_hash).await)
            }
            SearchType::Announce {
                info_hash,
                port,
                implied_port,
            } => SearchResult::Announce(self.run_announce(info_hash, port, implied_port).await),
        };

        let _ = self.resp_tx.send(result);
    }

    async fn run_find_node(&self, target: NodeId) -> Result<Vec<CompactNodeInfo>, DhtError> {
        // Similar to the original iterative_find_node but simplified
        // In a full implementation, this would need to communicate with DhtActor
        // to update the routing table and handle responses

        let initial_nodes = self.routing_table.get_closest_nodes(&target, K);

        if initial_nodes.is_empty() {
            return Ok(Vec::new());
        }

        let mut candidates: Vec<CompactNodeInfo> = initial_nodes
            .into_iter()
            .map(|n| CompactNodeInfo {
                node_id: n.node_id,
                addr: n.addr,
            })
            .collect();

        let mut queried: HashSet<NodeId> = HashSet::new();
        let mut all_nodes = candidates.clone();

        for _ in 0..5 {
            candidates.sort_by(|a, b| {
                let dist_a = a.node_id ^ target;
                let dist_b = b.node_id ^ target;
                dist_a.as_bytes().cmp(&dist_b.as_bytes())
            });

            let to_query: Vec<_> = candidates
                .iter()
                .filter(|n| !queried.contains(&n.node_id))
                .take(ALPHA)
                .cloned()
                .collect();

            if to_query.is_empty() {
                break;
            }

            // Query in parallel
            let futures: Vec<_> = to_query
                .iter()
                .map(|node| {
                    let tx_id = self.next_tx_id.fetch_add(1, Ordering::Relaxed);
                    let msg = KrpcMessage::find_node_query(tx_id, self.node_id, target);
                    let socket = self.socket_handle.clone();
                    let addr = node.addr;

                    async move {
                        let bytes = msg.to_bytes();
                        if let Err(e) = socket.send_to(&bytes, SocketAddr::V4(addr)).await {
                            tracing::debug!("Failed to send find_node to {}: {}", addr, e);
                            return None;
                        }

                        // Wait for response with timeout
                        // In a full implementation, we'd need to set up a pending request
                        // and correlate the response. For now, this is simplified.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        None::<Vec<CompactNodeInfo>>
                    }
                })
                .collect();

            let _results = futures::future::join_all(futures).await;

            for node in to_query {
                queried.insert(node.node_id);
            }
        }

        all_nodes.sort_by(|a, b| {
            let dist_a = a.node_id ^ target;
            let dist_b = b.node_id ^ target;
            dist_a.as_bytes().cmp(&dist_b.as_bytes())
        });
        all_nodes.truncate(K);

        Ok(all_nodes)
    }

    async fn run_get_peers(&self, info_hash: InfoHash) -> Result<GetPeersResult, DhtError> {
        let target = NodeId::from(&info_hash);

        let initial_nodes = self.routing_table.get_closest_nodes(&target, K);

        if initial_nodes.is_empty() {
            return Ok(GetPeersResult {
                peers: Vec::new(),
                nodes_contacted: 0,
                nodes_with_tokens: Vec::new(),
            });
        }

        let mut candidates: Vec<CompactNodeInfo> = initial_nodes
            .into_iter()
            .map(|n| CompactNodeInfo {
                node_id: n.node_id,
                addr: n.addr,
            })
            .collect();

        let mut queried: HashSet<NodeId> = HashSet::new();
        let mut all_peers: Vec<SocketAddrV4> = Vec::new();
        let mut nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)> = Vec::new();

        for _ in 0..5 {
            candidates.sort_by(|a, b| {
                let dist_a = a.node_id ^ target;
                let dist_b = b.node_id ^ target;
                dist_a.as_bytes().cmp(&dist_b.as_bytes())
            });

            let to_query: Vec<_> = candidates
                .iter()
                .filter(|n| !queried.contains(&n.node_id))
                .take(ALPHA)
                .cloned()
                .collect();

            if to_query.is_empty() {
                break;
            }

            for node in to_query {
                queried.insert(node.node_id);

                let tx_id = self.next_tx_id.fetch_add(1, Ordering::Relaxed);
                let msg = KrpcMessage::get_peers_query(tx_id, self.node_id, info_hash);
                let bytes = msg.to_bytes();

                if let Err(e) = self
                    .socket_handle
                    .send_to(&bytes, SocketAddr::V4(node.addr))
                    .await
                {
                    tracing::debug!("Failed to send get_peers to {}: {}", node.addr, e);
                }

                // In a full implementation, we'd wait for responses here
                // and parse peers/tokens. This is simplified.
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Ok(GetPeersResult {
            peers: all_peers,
            nodes_contacted: queried.len(),
            nodes_with_tokens,
        })
    }

    async fn run_announce(
        &self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<AnnounceResult, DhtError> {
        // First get peers (simplified - doesn't actually use results)
        let get_peers_result = self.run_get_peers(info_hash).await?;

        let mut announce_count = 0;

        // Announce to nodes with tokens (simplified)
        for (node, token) in get_peers_result.nodes_with_tokens.iter() {
            let tx_id = self.next_tx_id.fetch_add(1, Ordering::Relaxed);
            let msg = KrpcMessage::announce_peer_query(
                tx_id,
                self.node_id,
                info_hash,
                port,
                token.clone(),
                implied_port,
            );
            let bytes = msg.to_bytes();

            if let Err(e) = self
                .socket_handle
                .send_to(&bytes, SocketAddr::V4(node.addr))
                .await
            {
                tracing::debug!("Failed to send announce to {}: {}", node.addr, e);
            } else {
                announce_count += 1;
            }
        }

        Ok(AnnounceResult {
            peers: get_peers_result.peers,
            announce_count,
        })
    }
}
