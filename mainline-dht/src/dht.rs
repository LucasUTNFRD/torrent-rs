//! DHT node implementation for BitTorrent Mainline DHT (BEP 0005).
//!
//! This module provides the main DHT client with:
//! - Bootstrap into the DHT network
//! - Iterative node lookup (find_node)
//! - Server mode (responding to incoming queries)

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};

use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    time::timeout,
};

use crate::{
    error::DhtError,
    message::{CompactNodeInfo, KrpcMessage, MessageBody, Query, Response},
    node::Node,
    node_id::NodeId,
    routing_table::{K, RoutingTable},
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

struct DhtActor {
    socket: Arc<UdpSocket>,
    node_id: NodeId,
    routing_table: RoutingTable,
    command_rx: mpsc::Receiver<DhtCommand>,
    next_tx_id: AtomicU16,
    pending: HashMap<Vec<u8>, PendingRequest>,
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
                        DhtCommand::GetRoutingTableSize { resp } => {
                            let _ = resp.send(self.routing_table.node_count());
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
                        tracing::info!("Discovered external IP: {}", sender_ip.ip());
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
                        if let Some(resp_node_id) = msg.get_node_id() {
                            if !resp_node_id.is_node_id_secure(IpAddr::V4(*node.addr.ip())) {
                                tracing::debug!(
                                    "Non-compliant node ID from {} (soft enforcement)",
                                    node.addr
                                );
                            }
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
                if let Some(pending) = self.pending.remove(&tx_id) {
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

    async fn handle_query(&self, msg: &KrpcMessage, query: &Query, from: SocketAddr) {
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
        };

        let bytes = response.to_bytes();
        if let Err(e) = self.socket.send_to(&bytes, from).await {
            tracing::warn!("Failed to send response to {from}: {e}");
        }
    }
}
