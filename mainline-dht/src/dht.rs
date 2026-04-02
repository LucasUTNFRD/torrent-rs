//! DHT node rmplementation for BitTorrent Mainline DHT (BEP 0005).
//!
//! This module provides the main DHT client with:
//! - Bootstrap into the DHT network
//! - Iterative node lookup (find_node)
//! - Iterative peer lookup (get_peers)
//! - Announce peer participation (announce_peer)
//! - Server mode (responding to incoming queries)

use rand::RngExt;
use std::{
    collections::{HashMap, HashSet},
    fs,
    net::{Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use bencode::{Bencode, BencodeBuilder, BencodeDict};
use bittorrent_common::types::InfoHash;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    time::{Instant, interval},
};

use crate::{
    dht_config::{BootstrapNode, DhtConfig},
    error::DhtError,
    message::{CompactNodeInfo, KrpcMessage, MessageBody, Query, Response, TransactionId},
    metrics::{
        inc_bytes_in, inc_bytes_out, inc_msg_dropped, inc_msg_in, inc_msg_out, inc_msg_out_dropped,
        set_nodes, set_peers, set_torrents,
    },
    node::Node,
    node_id::NodeId,
    peer_store::PeerStore,
    routing_table::{K, RoutingTable},
    token::TokenManager,
    transaction::{QueryType, Transaction, TransactionManager},
};

/// Timeout for individual queries.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

// ============================================================================
// Result Types
// ============================================================================

/// Result of a get_peers lookup.
#[derive(Debug, Clone)]
pub struct GetPeersResult {
    /// Peers found for the infohash.
    pub peers: Vec<SocketAddr>,
    /// Number of nodes that responded.
    pub nodes_contacted: usize,
    /// Closest nodes with their tokens (for subsequent announce).
    pub nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)>,
}

/// Response from DHT operations, matching TrackerResponse pattern.
#[derive(Debug, Clone)]
pub struct DhtResponse {
    /// Peers found for the infohash.
    pub peers: Vec<SocketAddr>,
}

// ============================================================================
// Public API: DhtHandler
// ============================================================================

/// Lightweight handle for DHT operations.
///
/// This follows the same pattern as `TrackerHandler` from tracker-client,
/// providing a simple interface for peer discovery and announcement.
#[derive(Clone)]
pub struct DhtHandler {
    command_tx: mpsc::Sender<DhtCommand>,
    node_id: Arc<std::sync::RwLock<NodeId>>,
}

impl DhtHandler {
    /// Create a new DHT node with full configuration
    pub async fn with_config(config: DhtConfig) -> Result<Self, DhtError> {
        let bind_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), config.port);
        let socket = UdpSocket::bind(bind_addr).await?;
        let socket = Arc::new(socket);

        let initial_id = if let Some(ref path) = config.id_file_path {
            match NodeId::load_or_generate(path) {
                Ok(id) => {
                    tracing::info!("Loaded node ID from {}", path.display());
                    id
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to load node ID from {}: {}, generating new",
                        path.display(),
                        e
                    );
                    NodeId::generate_random()
                }
            }
        } else {
            NodeId::generate_random()
        };

        let node_id = Arc::new(std::sync::RwLock::new(initial_id));
        let (command_tx, command_rx) = mpsc::channel(32);

        let actor = DhtActor::new(
            socket,
            initial_id,
            command_rx,
            config.id_file_path.clone(),
            config.state_file_path.clone(),
            config.bootstrap_nodes,
        );

        let node_id_clone = node_id.clone();
        tokio::spawn(async move {
            if let Err(e) = actor.run(node_id_clone).await {
                tracing::error!("DHT actor error: {e}");
            }
        });

        Ok(DhtHandler {
            command_tx,
            node_id,
        })
    }

    // /// Bootstrap into the DHT network using default bootstrap nodes.
    // ///
    // /// Sends ping queries to bootstrap nodes to populate the routing table.
    // pub async fn bootstrap(&self) -> Result<(), DhtError> {
    //     self.bootstrap_with_nodes(&DEFAULT_BOOTSTRAP_NODES).await
    // }
    //
    // /// Bootstrap using custom bootstrap nodes.
    // pub async fn bootstrap_with_nodes(&self, nodes: &[&str]) -> Result<(), DhtError> {
    //     let (resp_tx, resp_rx) = oneshot::channel();
    //
    //     self.command_tx
    //         .send(DhtCommand::Bootstrap {
    //             nodes: nodes.iter().map(|s| s.to_string()).collect(),
    //             resp: resp_tx,
    //         })
    //         .await
    //         .map_err(|_| DhtError::ChannelClosed)?;
    //
    //     resp_rx.await.map_err(|_| DhtError::ChannelClosed)?
    // }

    // /// Find the K closest nodes to a target ID.
    // ///
    // /// Performs an iterative lookup, querying progressively closer nodes
    // /// until no closer nodes are found.
    // pub async fn find_node(&self, target: NodeId) -> Result<Vec<CompactNodeInfo>, DhtError> {
    //     let (resp_tx, resp_rx) = oneshot::channel();
    //
    //     self.command_tx
    //         .send(DhtCommand::FindNode {
    //             target,
    //             resp: resp_tx,
    //         })
    //         .await
    //         .map_err(|_| DhtError::ChannelClosed)?;
    //
    //     resp_rx.await.map_err(|_| DhtError::ChannelClosed)?
    // }

    /// Find peers for a torrent infohash.
    ///
    /// Performs a DHT lookup to discover peers for the given info_hash.
    /// Returns a receiver that streams peer addresses as they are discovered.
    pub async fn get_peers(&self, info_hash: InfoHash) -> mpsc::Receiver<Vec<SocketAddr>> {
        let (peer_tx, peer_rx) = mpsc::channel(32);

        let _ = self
            .command_tx
            .send(DhtCommand::GetPeers { info_hash, peer_tx })
            .await;

        peer_rx
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

    // ========================================================================
    // Announce API
    // ========================================================================

    /// Announce that we are participating in a torrent.
    ///
    /// Finds the closest nodes to the infohash in our routing table,
    /// sends get_peers to obtain tokens, then announces to those nodes.
    ///
    /// Note: This does NOT return peers. Call `get_peers` separately to
    /// discover peers you can connect to.
    pub async fn announce(&self, info_hash: InfoHash, port: u16) -> Result<usize, DhtError> {
        self.announce_inner(info_hash, port, false).await
    }

    /// Announce with implied_port for NAT traversal.
    ///
    /// The receiving DHT nodes will use our source UDP port as the peer port
    /// instead of the provided port value. Useful when behind NAT.
    pub async fn announce_implied(
        &self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<usize, DhtError> {
        self.announce_inner(info_hash, port, implied_port).await
    }

    /// Internal implementation for announce methods.
    async fn announce_inner(
        &self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<usize, DhtError> {
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

    // ========================================================================
    // Deprecated announce methods (kept for backward compatibility)
    // ========================================================================

    /// Announce that we are participating in a torrent.
    #[deprecated(since = "0.2.0", note = "Use `announce` instead")]
    pub async fn announce_peer(&self, info_hash: InfoHash, port: u16) -> Result<usize, DhtError> {
        self.announce_inner(info_hash, port, false).await
    }

    /// Announce with implied_port option for NAT traversal.
    #[deprecated(since = "0.2.0", note = "Use `announce_implied` instead")]
    pub async fn announce_peer_ext(
        &self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<usize, DhtError> {
        self.announce_inner(info_hash, port, implied_port).await
    }

    /// Announce with implied_port option for NAT traversal.
    #[deprecated(since = "0.2.0", note = "Use `announce_implied` instead")]
    pub async fn announce_ext(
        &self,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<usize, DhtError> {
        self.announce_inner(info_hash, port, implied_port).await
    }
}

// ============================================================================
// Internal: Commands
// ============================================================================

enum DhtCommand {
    // FindNode {
    //     target: NodeId,
    //     resp: oneshot::Sender<Result<Vec<CompactNodeInfo>, DhtError>>,
    // },
    GetPeers {
        info_hash: InfoHash,
        peer_tx: mpsc::Sender<Vec<SocketAddr>>,
    },
    Announce {
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
        resp: oneshot::Sender<Result<usize, DhtError>>,
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
// Internal: DHT Actor
// ============================================================================

struct DhtActor {
    socket: Arc<UdpSocket>,
    node_id: NodeId,
    routing_table: RoutingTable,
    command_rx: mpsc::Receiver<DhtCommand>,
    /// Storage for announced peers.
    peer_store: PeerStore,
    /// Token generation and validation.
    token_manager: TokenManager,
    /// Path to store the node ID file for persistence (BEP 42).
    #[allow(dead_code)]
    id_file_path: Option<PathBuf>,
    /// Path to store the DHT state (routing table) for persistence.
    state_file_path: Option<PathBuf>,
    transaction_manager: TransactionManager,
    /// Active get_peers lookups and their collected results
    pending_get_peers: HashMap<InfoHash, GetPeersLookupState>,
    /// Bootstrap nodes for DHT network entry.
    bootstrap_nodes: Vec<BootstrapNode>,
}

/// State for an active get_peers lookup
#[derive(Debug)]
struct GetPeersLookupState {
    /// Peers discovered so far
    peers: HashSet<SocketAddr>,
    /// Nodes with tokens for potential announce
    nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)>,
    /// Nodes we've queried
    #[allow(dead_code)]
    queried_nodes: HashSet<SocketAddr>,
    /// Channel to stream discovered peers to caller
    peer_tx: mpsc::Sender<Vec<SocketAddr>>,
    /// When the lookup started
    #[allow(dead_code)]
    started_at: Instant,
}

impl DhtActor {
    fn new(
        socket: Arc<UdpSocket>,
        node_id: NodeId,
        command_rx: mpsc::Receiver<DhtCommand>,
        id_file_path: Option<PathBuf>,
        state_file_path: Option<PathBuf>,
        bootstrap_nodes: Vec<BootstrapNode>,
    ) -> Self {
        Self {
            socket,
            node_id,
            routing_table: RoutingTable::new(node_id),
            command_rx,
            peer_store: PeerStore::new(),
            token_manager: TokenManager::new(),
            id_file_path,
            state_file_path,
            transaction_manager: TransactionManager::new(),
            pending_get_peers: std::collections::HashMap::new(),
            bootstrap_nodes,
        }
    }

    const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(10);
    const BOOTSTRAP_TARGET: usize = 180;

    /// Resolve bootstrap node addresses from DNS.
    /// This is done upfront because DNS resolution can block.
    fn resolve_bootstrap_nodes(&self) -> Result<Vec<SocketAddr>, DhtError> {
        let mut addrs: Vec<SocketAddr> = Vec::new();
        for node in &self.bootstrap_nodes {
            let host_port = format!("{}:{}", node.host, node.port);
            match host_port.to_socket_addrs() {
                Ok(resolved) => {
                    addrs.extend(resolved.filter(|a| a.is_ipv4()));
                }
                Err(e) => tracing::warn!("Failed to resolve {}: {}", host_port, e),
            }
        }

        if addrs.is_empty() {
            tracing::warn!("No bootstrap nodes could be resolved");
        }

        Ok(addrs)
    }

    /// Perform blocking bootstrap until we have enough nodes or timeout.
    /// This sends pings to bootstrap nodes and processes responses inline.
    async fn do_bootstrap(&mut self, bootstrap_nodes: &[SocketAddr]) -> Result<(), DhtError> {
        tracing::info!(
            "Starting DHT bootstrap with {} bootstrap nodes",
            bootstrap_nodes.len()
        );

        // Load persisted state for faster bootstrap
        if let Some(path) = self.state_file_path.clone()
            && let Some((_saved_id, saved_nodes)) = Self::load_state(&path)
        {
            tracing::info!("Loaded {} persisted nodes from cache", saved_nodes.len());
            for node_info in &saved_nodes {
                self.routing_table
                    .try_add_node(Node::new(node_info.node_id, node_info.addr));
            }
        }

        if self.routing_table.node_count() >= Self::BOOTSTRAP_TARGET {
            tracing::info!(
                "Bootstrap complete: {} nodes from persisted state",
                self.routing_table.node_count()
            );
            return Ok(());
        }

        if bootstrap_nodes.is_empty() && self.routing_table.node_count() == 0 {
            tracing::error!("No bootstrap nodes available and routing table is empty");
            return Err(DhtError::BootstrapFailed);
        }

        tracing::debug!(
            "Bootstrap target: {} nodes, timeout: {:?}",
            Self::BOOTSTRAP_TARGET,
            Self::BOOTSTRAP_TIMEOUT
        );

        // Send find_node to all bootstrap nodes
        for &addr in bootstrap_nodes {
            tracing::debug!(
                "Sending find_node({}) to bootstrap node {}",
                self.node_id,
                addr
            );
            let _ = self.send_find_node(addr, self.node_id);
        }

        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Self::BOOTSTRAP_TIMEOUT;
        let mut last_progress_log = Instant::now();

        tracing::debug!("Entering bootstrap receive loop");

        while self.routing_table.node_count() < Self::BOOTSTRAP_TARGET {
            tokio::select! {
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((size, from)) => {
                            inc_msg_in();
                            inc_bytes_in(size);
                            let before_count = self.routing_table.node_count();
                            self.handle_incoming(&buf[..size], from).await;
                            let after_count = self.routing_table.node_count();

                            // Log progress when nodes are added
                            if after_count > before_count {
                                tracing::debug!(
                                    "Bootstrap progress: {} -> {} nodes (+{})",
                                    before_count,
                                    after_count,
                                    after_count - before_count
                                );
                            }

                            // Periodic progress logging (every 2 seconds)
                            if last_progress_log.elapsed().as_secs() >= 2 {
                                tracing::info!(
                                    "Bootstrap status: {}/{} nodes, {:.0}s remaining",
                                    after_count,
                                    Self::BOOTSTRAP_TARGET,
                                    (deadline - Instant::now()).as_secs_f32()
                                );
                                last_progress_log = Instant::now();
                            }
                        }
                        Err(e) => {
                            tracing::warn!("UDP recv error: {e}");
                        }
                    }
                },
                _ = tokio::time::sleep_until(deadline) => {
                    tracing::warn!(
                        "Bootstrap timed out after {:?}, routing table has {} nodes",
                        Self::BOOTSTRAP_TIMEOUT,
                        self.routing_table.node_count()
                    );
                    break;
                }
            }
        }

        let final_count = self.routing_table.node_count();
        if final_count >= Self::BOOTSTRAP_TARGET {
            tracing::info!(
                "Bootstrap target reached: {} nodes in routing table",
                final_count
            );
        } else if final_count == 0 {
            tracing::error!("Bootstrap failed: no nodes responded");
            return Err(DhtError::BootstrapFailed);
        } else {
            tracing::warn!(
                "Bootstrap finished with {} nodes (target: {})",
                final_count,
                Self::BOOTSTRAP_TARGET
            );
        }

        Ok(())
    }

    async fn run(
        mut self,
        _shared_node_id: Arc<std::sync::RwLock<NodeId>>,
    ) -> Result<(), DhtError> {
        let bootstrap_addrs = self.resolve_bootstrap_nodes()?;

        // Phase 1: Blocking bootstrap
        if !bootstrap_addrs.is_empty() {
            self.do_bootstrap(&bootstrap_addrs).await?;
        }

        let mut buf = [0u8; 4096];
        // Check for transaction timeouts every 500ms
        let mut timeout_interval = interval(Duration::from_millis(500));
        // Run DHT maintenance every 5 seconds
        let mut maintenance_interval = interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                // Handle incoming UDP packets
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((size, from)) => {
                            inc_msg_in();
                            inc_bytes_in(size);
                            self.handle_incoming(&buf[..size], from).await;
                        }
                        Err(e) => {
                            tracing::warn!("UDP recv error: {e}");
                        }
                    }
                }

                // Periodic DHT maintenance
                _ = maintenance_interval.tick() => {
                    self.perform_maintenance();
                    set_nodes(self.routing_table.node_count());
                    set_torrents(self.peer_store.torrent_count());
                    set_peers(self.peer_store.peer_count());
                }

                // Handle commands from the public API
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        // DhtCommand::FindNode { target, resp } => {
                        //     // Perform iterative find_node lookup
                        //     let result = self.iterative_find_node(target).await;
                        //     let _ = resp.send(result);
                        // }
                        DhtCommand::GetPeers { info_hash, peer_tx } => {
                            self.start_get_peers(info_hash, peer_tx).await;
                        }
                        DhtCommand::Announce { info_hash, port, implied_port, resp } => {
                            // Find closest nodes and send get_peers to get tokens, then announce
                            let target = NodeId::from(info_hash);
                            let closest: Vec<_> = self.routing_table.get_closest_nodes(&target, K)
                                .iter()
                                .map(|&n| n.addr)
                                .collect();

                            if closest.is_empty() {
                                let _ = resp.send(Err(DhtError::BootstrapFailed));
                                continue;
                            }

                            let node_count = closest.len();

                            // Send get_peers to closest nodes with announce context
                            for addr in closest {
                                let _ = self.send_get_peers_with_announce(
                                    addr,
                                    info_hash,
                                    port,
                                    implied_port
                                );
                            }

                            // Return the number of nodes we're trying to announce to
                            let _ = resp.send(Ok(node_count));
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
                            self.transaction_manager.clear();
                            self.save_state();
                            return Ok(());
                        }
                    }
                }

                // Check for transaction timeouts periodically
                _ = timeout_interval.tick() => {
                    let scan_result = self.transaction_manager.scan_timeouts(QUERY_TIMEOUT, 2);

                    // Retry timed-out transactions
                    for tx in scan_result.to_retry {
                        tracing::debug!("Retrying {} to {}", tx.query_type_str(), tx.addr);
                        self.transaction_manager.mark_retry(&tx.tx_id);

                        // Resend based on query type
                        match &tx.query_type {
                            QueryType::Ping => {
                                let msg = KrpcMessage::ping_query(
                                    u16::from_be_bytes(tx.tx_id.0),
                                    self.node_id
                                );
                                let bytes = msg.to_bytes();
                                let _ = self.socket.try_send_to(&bytes, tx.addr);
                                inc_msg_out();
                                inc_bytes_out(bytes.len());
                            }
                            QueryType::FindNode { target } => {
                                let msg = KrpcMessage::find_node_query(
                                    u16::from_be_bytes(tx.tx_id.0),
                                    self.node_id,
                                    *target
                                );
                                let bytes = msg.to_bytes();
                                let _ = self.socket.try_send_to(&bytes, tx.addr);
                                inc_msg_out();
                                inc_bytes_out(bytes.len());
                            }
                            QueryType::GetPeers { info_hash } => {
                                let msg = KrpcMessage::get_peers_query(
                                    u16::from_be_bytes(tx.tx_id.0),
                                    self.node_id,
                                    *info_hash
                                );
                                let bytes = msg.to_bytes();
                                let _ = self.socket.try_send_to(&bytes, tx.addr);
                                inc_msg_out();
                                inc_bytes_out(bytes.len());
                            }
                            _ => {}
                        }
                    }

                    // Remove transactions that exceeded max retries
                    for tx_id in scan_result.to_remove {
                        if let Some(tx) = self.transaction_manager.get_by_trans_id(&tx_id) {
                            tracing::debug!("Transaction {} to {} exceeded max retries",
                                u16::from_be_bytes(tx.tx_id.0), tx.addr);
                            // Optionally blacklist the node
                            if tx.query_type_str() != "ping" {
                                // Could add to blacklist here
                            }
                        }
                        self.transaction_manager.finish_by_trans_id(&tx_id);
                    }
                }
            }
        }
    }

    // ========================================================================
    // Get Peers (Iterative Lookup)
    // ========================================================================

    /// Perform an iterative find_node lookup.
    /// This queries the closest nodes to the target and returns them.
    async fn iterative_find_node(
        &mut self,
        target: NodeId,
    ) -> Result<Vec<CompactNodeInfo>, DhtError> {
        use std::collections::HashSet;

        let mut queried_nodes: HashSet<SocketAddr> = HashSet::new();
        let mut closest_nodes: Vec<CompactNodeInfo> = self
            .routing_table
            .get_closest_nodes(&target, K)
            .iter()
            .map(|&n| CompactNodeInfo {
                node_id: n.node_id,
                addr: n.addr,
            })
            .collect();

        if closest_nodes.is_empty() {
            return Ok(Vec::new());
        }

        // Iterative lookup - query nodes to find closer ones
        let max_iterations = 5;
        for _iteration in 0..max_iterations {
            let nodes_to_query: Vec<_> = closest_nodes
                .iter()
                .filter(|n| !queried_nodes.contains(&n.addr))
                .take(3) // Query alpha=3 nodes in parallel per iteration
                .cloned()
                .collect();

            if nodes_to_query.is_empty() {
                break;
            }

            // Send find_node to each node
            for node_info in nodes_to_query {
                if queried_nodes.contains(&node_info.addr) {
                    continue;
                }

                queried_nodes.insert(node_info.addr);

                // Send find_node query
                let _ = self.send_find_node(node_info.addr, target);
            }

            // Wait for responses
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Get updated closest nodes
            closest_nodes = self
                .routing_table
                .get_closest_nodes(&target, K)
                .iter()
                .map(|&n| CompactNodeInfo {
                    node_id: n.node_id,
                    addr: n.addr,
                })
                .collect();
        }

        Ok(closest_nodes)
    }

    /// Start a get_peers lookup - fires queries and streams results via channel.
    /// Unlike the old iterative approach, this just initiates queries once.
    /// Peers are streamed to the caller as responses arrive via the response handler.
    async fn start_get_peers(
        &mut self,
        info_hash: InfoHash,
        peer_tx: mpsc::Sender<Vec<SocketAddr>>,
    ) {
        use std::collections::HashSet;

        let target = NodeId::from(info_hash);

        let lookup_state = GetPeersLookupState {
            peers: HashSet::new(),
            nodes_with_tokens: Vec::new(),
            queried_nodes: HashSet::new(),
            peer_tx,
            started_at: Instant::now(),
        };
        self.pending_get_peers.insert(info_hash, lookup_state);

        let closest_nodes: Vec<_> = self
            .routing_table
            .get_closest_nodes(&target, K)
            .iter()
            .map(|&n| CompactNodeInfo {
                node_id: n.node_id,
                addr: n.addr,
            })
            .collect();

        if closest_nodes.is_empty() {
            tracing::info!("Routing table empty, querying bootstrap nodes for get_peers");

            let bootstrap_nodes: Vec<_> = self
                .bootstrap_nodes
                .iter()
                .flat_map(|node| {
                    let host_port = format!("{}:{}", node.host, node.port);
                    match host_port.to_socket_addrs() {
                        Ok(addrs) => addrs.filter(|a| a.is_ipv4()).collect::<Vec<_>>(),
                        Err(_) => Vec::new(),
                    }
                })
                .collect();

            for addr in bootstrap_nodes {
                let _ = self.send_get_peers(addr, info_hash);
            }
        } else {
            for node_info in closest_nodes.iter().take(3) {
                let _ = self.send_get_peers(node_info.addr, info_hash);
            }
        }
    }

    // ========================================================================
    // Announce
    // ========================================================================

    /// Send get_peers query with context to trigger announce after receiving token.
    fn send_get_peers_with_announce(
        &mut self,
        addr: SocketAddr,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("get_peers", &addr)
            .is_some()
        {
            return Ok(());
        }

        // Create transaction with announce context
        let mut tx = Transaction::new(tx_id.clone(), addr, QueryType::GetPeers { info_hash });
        tx.announce_port = Some(port);
        tx.implied_port = implied_port;

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg =
            KrpcMessage::get_peers_query(u16::from_be_bytes(tx_id.0), self.node_id, info_hash);
        let bytes = msg.to_bytes();

        match self.socket.try_send_to(&bytes, addr) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.transaction_manager.finish_by_trans_id(&tx_id);
                Err(DhtError::Network(e))
            }
        }
    }

    // ========================================================================
    // Send queries
    // ========================================================================

    /// Send a ping query without awaiting response.
    /// Response will be processed in handle_incoming when it arrives.
    fn send_ping(&mut self, addr: SocketAddr) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("ping", &addr)
            .is_some()
        {
            return Ok(());
        }

        // Create and register transaction
        let tx = Transaction::new(tx_id.clone(), addr, QueryType::Ping);

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg = KrpcMessage::ping_query(u16::from_be_bytes(tx_id.0), self.node_id);
        self.send_query(msg, addr, tx_id)
    }

    /// Send a find_node query without awaiting response.
    fn send_find_node(&mut self, addr: SocketAddr, target: NodeId) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("find_node", &addr)
            .is_some()
        {
            return Ok(());
        }

        // Create and register transaction
        let tx = Transaction::new(tx_id.clone(), addr, QueryType::FindNode { target });

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg = KrpcMessage::find_node_query(u16::from_be_bytes(tx_id.0), self.node_id, target);
        self.send_query(msg, addr, tx_id)
    }

    /// Send a get_peers query without awaiting response.
    fn send_get_peers(&mut self, addr: SocketAddr, info_hash: InfoHash) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("get_peers", &addr)
            .is_some()
        {
            return Ok(());
        }

        // Create and register transaction
        let tx = Transaction::new(tx_id.clone(), addr, QueryType::GetPeers { info_hash });

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg =
            KrpcMessage::get_peers_query(u16::from_be_bytes(tx_id.0), self.node_id, info_hash);
        self.send_query(msg, addr, tx_id)
    }

    /// Send an announce_peer query without awaiting response.
    fn send_announce_peer(
        &mut self,
        addr: SocketAddr,
        info_hash: InfoHash,
        port: u16,
        token: Vec<u8>,
        implied_port: bool,
    ) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Create and register transaction (no duplicate check needed for announce)
        let tx = Transaction::new(
            tx_id.clone(),
            addr,
            QueryType::AnnouncePeer {
                info_hash,
                port,
                implied_port,
            },
        );

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg = KrpcMessage::announce_peer_query(
            u16::from_be_bytes(tx_id.0),
            self.node_id,
            info_hash,
            port,
            token,
            implied_port,
        );
        self.send_query(msg, addr, tx_id)
    }

    fn send_query(
        &mut self,
        msg: KrpcMessage,
        addr: SocketAddr,
        tx_id: TransactionId,
    ) -> Result<(), DhtError> {
        let bytes = msg.to_bytes();
        match self.socket.try_send_to(&bytes, addr) {
            Ok(_) => {
                inc_msg_out();
                inc_bytes_out(bytes.len());
                Ok(())
            }
            Err(e) => {
                self.transaction_manager.finish_by_trans_id(&tx_id);
                inc_msg_out_dropped();
                Err(DhtError::Network(e))
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
                inc_msg_dropped();
                return;
            }
        };

        match &msg.body {
            MessageBody::Query(query) => {
                tracing::debug!("Received query from {from}");
                self.handle_query(&msg, query, from).await;
            }
            MessageBody::Response(response) => {
                // Look up the pending transaction
                let tx_id = TransactionId(msg.transaction_id.0);

                if let Some(tx) = self.transaction_manager.get_by_trans_id(&tx_id).cloned() {
                    // Process response based on query type
                    match &tx.query_type {
                        QueryType::Ping => {
                            // Extract node ID from response
                            if let Some(node_id) = msg.get_node_id() {
                                tracing::trace!("Ping response from {}: node {:?}", from, node_id);
                                // Update routing table
                                let node = Node::new_good(node_id, from);
                                self.routing_table.try_add_node(node);

                                // Idk if this is questionable but  on ping response we:
                                //  Learn the node's ID
                                //  if we do self_find_node it gets us  K closest nodes to us (cascade discovery)
                                let _ = self.send_find_node(from, self.node_id);
                            }
                        }
                        QueryType::FindNode { target } => {
                            if let Response::FindNode { nodes, .. } = response {
                                tracing::debug!(
                                    "FindNode response from {}: {} nodes returned for target {:?}",
                                    from,
                                    nodes.len(),
                                    target
                                );
                                let before_count = self.routing_table.node_count();
                                for node_info in nodes {
                                    // Add to routing table
                                    let new_node = Node::new(node_info.node_id, node_info.addr);
                                    self.routing_table.try_add_node(new_node);

                                    // Continue searching toward target
                                    // Only if we haven't already queried this node
                                    if self
                                        .transaction_manager
                                        .get_by_index("find_node", &node_info.addr)
                                        .is_none()
                                    {
                                        tracing::trace!(
                                            "Querying discovered node {} for target {:?}",
                                            node_info.addr,
                                            target
                                        );
                                        let _ = self.send_find_node(node_info.addr, *target);
                                    }
                                }
                                let added = self.routing_table.node_count() - before_count;
                                if added > 0 {
                                    tracing::debug!(
                                        "Added {} new nodes to routing table from FindNode response",
                                        added
                                    );
                                }
                            }
                        }
                        QueryType::GetPeers { info_hash } => {
                            if let Response::GetPeers {
                                values,
                                nodes,
                                token,
                                ..
                            } = response
                            {
                                let peer_count = values.as_ref().map(|v| v.len()).unwrap_or(0);
                                let node_count = nodes.as_ref().map(|n| n.len()).unwrap_or(0);
                                tracing::debug!(
                                    "GetPeers response from {}: {} peers, {} closer nodes for {:?}",
                                    from,
                                    peer_count,
                                    node_count,
                                    info_hash
                                );

                                // Get the sender's node info from the transaction
                                let sender_node_info = {
                                    // Try to get node ID from response or transaction
                                    let node_id = msg.get_node_id().unwrap_or(self.node_id);
                                    Some(CompactNodeInfo {
                                        node_id,
                                        addr: from,
                                    })
                                };

                                // Update pending lookup state and stream peers to caller
                                if let Some(state) = self.pending_get_peers.get_mut(info_hash) {
                                    if let Some(peers) = values {
                                        let peer_vec: Vec<SocketAddr> = peers
                                            .iter()
                                            .filter(|p| state.peers.insert(**p))
                                            .copied()
                                            .collect();

                                        if !peer_vec.is_empty() {
                                            tracing::debug!(
                                                "Streaming {} peers to caller for {:?}",
                                                peer_vec.len(),
                                                info_hash
                                            );
                                            let _ = state.peer_tx.send(peer_vec).await;
                                        }
                                    }
                                    if let Some(ref node_info) = sender_node_info {
                                        state
                                            .nodes_with_tokens
                                            .push((node_info.clone(), token.clone()));
                                    }
                                }

                                if let Some(peers) = values {
                                    // Found peers - add to peer store
                                    for peer_addr in peers {
                                        self.peer_store.add_peer(*info_hash, *peer_addr);
                                    }
                                }
                                if let Some(node_list) = nodes {
                                    // No peers found, query closer nodes
                                    for node_info in node_list {
                                        // Add to routing
                                        let new_node = Node::new(node_info.node_id, node_info.addr);
                                        self.routing_table.try_add_node(new_node);

                                        // Send get_peers to this node
                                        if self
                                            .transaction_manager
                                            .get_by_index("get_peers", &node_info.addr)
                                            .is_none()
                                        {
                                            tracing::trace!(
                                                "Querying closer node {} for {:?}",
                                                node_info.addr,
                                                info_hash
                                            );
                                            let _ = self.send_get_peers(node_info.addr, *info_hash);
                                        }
                                    }
                                }

                                // If we have an announce_port set, send announce_peer
                                if let Some(port) = tx.announce_port {
                                    tracing::debug!(
                                        "Sending announce_peer to {} for {:?} on port {}",
                                        from,
                                        info_hash,
                                        port
                                    );
                                    let _ = self.send_announce_peer(
                                        from,
                                        *info_hash,
                                        port,
                                        token.clone(),
                                        tx.implied_port,
                                    );
                                }
                            }
                        }
                        QueryType::AnnouncePeer { .. } => {
                            // Announce completed - nothing special to do
                            tracing::debug!("Announce peer completed to {}", from);
                        }
                    }

                    // Mark transaction as completed
                    self.transaction_manager.finish_by_trans_id(&tx_id);
                } else {
                    tracing::debug!("Received response for unknown transaction from {}", from);
                }
            }
            MessageBody::Error { code, message } => {
                tracing::debug!("Received error from {from}: [{code}] {message}");
            }
        }
    }

    async fn handle_query(&mut self, msg: &KrpcMessage, query: &Query, from: SocketAddr) {
        // BEP 05: When we receive a query, update the node's status
        // If the node has previously responded, receiving a query from it keeps it "good"
        if let Some(id) = msg.get_node_id() {
            // Try to add/update the node in routing table
            let node = Node::new(id, from);
            self.routing_table.try_add_node(node);
            // Mark that we received a query from this node
            self.routing_table.mark_node_query_from(&id);
        }

        let response = match query {
            Query::Ping { .. } => {
                tracing::trace!("Received ping query from {}", from);
                KrpcMessage::ping_response(msg.transaction_id.clone(), self.node_id)
            }
            Query::FindNode { target, .. } => {
                tracing::debug!(
                    "Received find_node query from {} for target {:?}",
                    from,
                    target
                );
                let closest = self.routing_table.get_closest_nodes(target, K);
                let count = closest.len();
                let nodes: Vec<CompactNodeInfo> = closest
                    .into_iter()
                    .map(|n| CompactNodeInfo {
                        node_id: n.node_id,
                        addr: n.addr,
                    })
                    .collect();
                tracing::debug!(
                    "Responding with {} nodes for find_node from {}",
                    count,
                    from
                );
                KrpcMessage::find_node_response(msg.transaction_id.clone(), self.node_id, nodes)
            }
            Query::GetPeers { info_hash, .. } => {
                tracing::debug!("Received get_peers query from {} for {:?}", from, info_hash);
                self.handle_get_peers_query(msg.transaction_id.clone(), from, info_hash)
            }
            Query::AnnouncePeer {
                info_hash,
                port,
                token,
                implied_port,
                ..
            } => {
                tracing::debug!(
                    "Received announce_peer query from {} for {:?} port {}",
                    from,
                    info_hash,
                    port
                );
                self.handle_announce_peer_query(
                    msg.transaction_id.clone(),
                    from,
                    *info_hash,
                    *port,
                    token,
                    *implied_port,
                )
            }
        };

        let bytes = response.to_bytes();
        if let Err(e) = self.socket.send_to(&bytes, from).await {
            tracing::warn!("Failed to send response to {from}: {e}");
        } else {
            inc_msg_out();
            inc_bytes_out(bytes.len());
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
        let peer_addr = if implied_port {
            from
        } else {
            SocketAddr::new(from.ip(), port)
        };

        // Store the peer
        self.peer_store.add_peer(info_hash, peer_addr);
        tracing::debug!("Stored peer {} for {}", peer_addr, info_hash);

        KrpcMessage::announce_peer_response(tx_id, self.node_id)
    }

    /// Save DHT state (node_id + routing table nodes) to disk as a bencode dict.
    ///
    /// Format:
    /// ```text
    /// d
    ///   2:id  20:<node_id bytes>
    ///   5:nodes <compact node info: 26 bytes per node>
    /// e
    /// ```
    fn save_state(&self) {
        let Some(ref path) = self.state_file_path else {
            return;
        };

        let nodes = self.routing_table.get_all_nodes();
        let compact_nodes = crate::message::encode_compact_nodes_v4(
            &nodes
                .iter()
                .map(|n| CompactNodeInfo {
                    node_id: n.node_id,
                    addr: n.addr,
                })
                .collect::<Vec<_>>(),
        );

        let mut dict = std::collections::BTreeMap::<Vec<u8>, Bencode>::new();
        dict.put("id", &self.node_id.as_bytes().as_slice());
        dict.put("nodes", &compact_nodes.as_slice());
        let encoded = Bencode::encoder(&dict.build());

        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            tracing::warn!(
                "Failed to create state directory {}: {}",
                parent.display(),
                e
            );
            return;
        }

        match fs::write(path, &encoded) {
            Ok(()) => tracing::info!(
                "Saved DHT state: {} nodes to {}",
                nodes.len(),
                path.display()
            ),
            Err(e) => tracing::warn!("Failed to save DHT state to {}: {}", path.display(), e),
        }
    }

    /// Load persisted DHT state from disk.
    /// Returns (node_id, list of compact node infos) if successful.
    fn load_state(path: &PathBuf) -> Option<(NodeId, Vec<CompactNodeInfo>)> {
        let data = match fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!("No DHT state file at {}: {}", path.display(), e);
                return None;
            }
        };

        let decoded = match Bencode::decode(&data) {
            Ok(Bencode::Dict(dict)) => dict,
            _ => {
                tracing::warn!("Invalid DHT state file at {}", path.display());
                return None;
            }
        };

        let id_bytes = decoded.get_bytes(b"id")?;
        if id_bytes.len() != 20 {
            tracing::warn!("Invalid node ID length in DHT state file");
            return None;
        }
        let mut id_arr = [0u8; 20];
        id_arr.copy_from_slice(id_bytes);
        let node_id = NodeId::from_bytes(id_arr);

        let nodes_bytes = decoded.get_bytes(b"nodes")?;
        let nodes = match crate::message::decode_compact_nodes_v4(nodes_bytes) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("Failed to decode nodes from DHT state: {}", e);
                return None;
            }
        };

        tracing::info!(
            "Loaded DHT state: node_id={:?}, {} nodes from {}",
            node_id,
            nodes.len(),
            path.display()
        );

        Some((node_id, nodes))
    }

    /// Try to add a node to the routing table by pinging it first.
    ///
    /// Per BEP 0005: Peers that receive a PORT message should attempt to ping
    /// the node on the received port and IP address. If a response is received,
    /// the node should be inserted into the routing table according to the usual rules.
    async fn try_add_node(&mut self, addr: SocketAddr) -> Result<(), DhtError> {
        self.send_ping(addr)
    }

    // ========================================================================
    // DHT Maintenance
    // ========================================================================

    /// Perform periodic DHT maintenance tasks:
    /// 1. Bucket Maintenance: Trigger find_node for stale buckets
    /// 2. Neighborhood Maintenance: Aggressive for our own bucket
    /// 3. Proactive Recovery: Fill empty buckets, random recovery
    fn perform_maintenance(&mut self) {
        let mut rng = rand::rng();

        // 1. Bucket Maintenance: Trigger find_node for stale buckets
        // Rule: If bucket hasn't been updated in timeout
        let buckets_to_maintain: Vec<_> = self
            .routing_table
            .buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.nodes.is_empty() && b.needs_maintenance())
            .map(|(i, _)| i)
            .collect();

        for bucket_index in buckets_to_maintain {
            // Pick random ID in this bucket's range
            let target_id = self.routing_table.random_id_in_bucket_range(bucket_index);

            // Get a random node from this bucket or a neighbor
            if let Some(node) = self
                .routing_table
                .get_random_node_from_bucket(bucket_index)
                .or_else(|| self.routing_table.get_random_node())
            {
                tracing::debug!(
                    "Bucket {} maintenance: find_node to {} for target {:?}",
                    bucket_index,
                    node.addr,
                    target_id
                );
                let _ = self.send_find_node(node.addr, target_id);
            }
        }

        // 2. Neighborhood Maintenance (Aggressive for our own bucket - bucket 0)
        // Rule: If your bucket is "growing" (recently split/updated within 150s)
        let own_bucket = self.routing_table.get_own_bucket();
        if own_bucket.is_growing() {
            // Every 5-15 seconds (we're called every 5s, so this is fine)
            let target_id = self.routing_table.random_id_in_own_bucket();

            if let Some(node) = self
                .routing_table
                .get_random_node_from_bucket(0)
                .or_else(|| self.routing_table.get_random_node())
            {
                tracing::debug!(
                    "Neighborhood maintenance: find_node to {} for target {:?}",
                    node.addr,
                    target_id
                );
                let _ = self.send_find_node(node.addr, target_id);
            }
        }

        // 3. Proactive Recovery
        // - If a bucket is empty (always try to fill it)
        let empty_buckets: Vec<_> = self
            .routing_table
            .buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| b.nodes.is_empty())
            .map(|(i, _)| i)
            .collect();

        for bucket_index in empty_buckets {
            let target_id = self.routing_table.random_id_in_bucket_range(bucket_index);

            if let Some(node) = self.routing_table.get_random_node() {
                tracing::debug!(
                    "Proactive recovery for empty bucket {}: find_node to {} for target {:?}",
                    bucket_index,
                    node.addr,
                    target_id
                );
                let _ = self.send_find_node(node.addr, target_id);
            }
        }

        // - Randomly (1 in 8 chance) to recover from buckets full of broken nodes
        if rng.random_range(0..8) == 0
            && let Some(bucket_index) = (0..160).find(|_| true)
        {
            let target_id = self.routing_table.random_id_in_bucket_range(bucket_index);

            if let Some(node) = self.routing_table.get_random_node() {
                tracing::debug!(
                    "Random proactive recovery: find_node to {} for target {:?}",
                    node.addr,
                    target_id
                );
                let _ = self.send_find_node(node.addr, target_id);
            }
        }
    }
}
