//! DHT node implementation for BitTorrent Mainline DHT (BEP 0005).
//!
//! This module provides the main DHT client with:
//! - Bootstrap into the DHT network
//! - Iterative node lookup (find_node)
//! - Iterative peer lookup (get_peers)
//! - Announce peer participation (announce_peer)
//! - Server mode (responding to incoming queries)

use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use bencode::{Bencode, BencodeBuilder, BencodeDict};
use bittorrent_common::types::InfoHash;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    time::interval,
};

use crate::{
    error::DhtError,
    message::{CompactNodeInfo, KrpcMessage, MessageBody, Query, Response, TransactionId},
    node::Node,
    node_id::NodeId,
    peer_store::PeerStore,
    routing_table::{K, RoutingTable},
    token::TokenManager,
    transaction::{QueryType, Transaction, TransactionManager},
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

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for DHT node ID persistence
#[derive(Debug, Clone)]
pub struct DhtConfig {
    /// Path to store the node ID file. If None, generates random ID on each start
    pub id_file_path: Option<PathBuf>,
    /// Path to store the DHT state (routing table nodes). If None, no state persistence
    pub state_file_path: Option<PathBuf>,
    /// Network port to bind to
    pub port: u16,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            id_file_path: None,
            state_file_path: None,
            port: DEFAULT_PORT,
        }
    }
}

impl DhtConfig {
    /// Create a config with default persistence in the user's config directory
    ///
    /// On Linux: ~/.config/mainline-dht/
    /// On macOS: ~/Library/Application Support/mainline-dht/
    /// On Windows: %APPDATA%/mainline-dht/
    pub fn with_default_persistence(port: u16) -> Result<Self, DhtError> {
        let project_dirs = directories::ProjectDirs::from("com", "mainline", "mainline-dht")
            .ok_or_else(|| DhtError::Other("Could not determine config directory".to_string()))?;

        let config_dir = project_dirs.config_dir();
        let id_path = config_dir.join("node.id");
        let state_path = config_dir.join("dht_state.dat");

        Ok(Self {
            id_file_path: Some(id_path),
            state_file_path: Some(state_path),
            port,
        })
    }
}

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
    /// Create a new DHT node bound to the specified port.
    ///
    /// If no port is specified, binds to port 6881 (default DHT port).
    /// The node is not bootstrapped yet - call `bootstrap()` to join the network.
    ///
    /// Uses default configuration (random ID on each start, no persistence).
    pub async fn new(port: Option<u16>) -> Result<Self, DhtError> {
        let config = DhtConfig {
            port: port.unwrap_or(DEFAULT_PORT),
            ..Default::default()
        };
        Self::with_config(config).await
    }

    /// Create a new DHT node with full configuration
    pub async fn with_config(config: DhtConfig) -> Result<Self, DhtError> {
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, config.port);
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

    /// Bootstrap into the DHT network using default bootstrap nodes.
    ///
    /// Sends ping queries to bootstrap nodes to populate the routing table.
    pub async fn bootstrap(&self) -> Result<(), DhtError> {
        self.bootstrap_with_nodes(&DEFAULT_BOOTSTRAP_NODES).await
    }

    /// Bootstrap using custom bootstrap nodes.
    pub async fn bootstrap_with_nodes(&self, nodes: &[&str]) -> Result<(), DhtError> {
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
    /// Starts a DHT lookup to discover peers for the given info_hash.
    /// Returns a receiver that streams peer addresses as they are discovered.
    pub async fn get_peers(&self, info_hash: InfoHash) -> mpsc::Receiver<Vec<SocketAddrV4>> {
        let (peer_tx, peer_rx) = mpsc::channel(32);

        let _ = self.command_tx.send(DhtCommand::GetPeers {
            info_hash,
            peer_tx,
        }).await;

        peer_rx
    }

    /// Announce that we are participating in a torrent.
    ///
    /// Finds the closest nodes to the infohash in our routing table,
    /// sends get_peers to obtain tokens, then announces to those nodes.
    ///
    /// Note: This does NOT return peers. Call `get_peers` separately to
    /// discover peers you can connect to.
    pub async fn announce_peer(&self, info_hash: InfoHash, port: u16) -> Result<usize, DhtError> {
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

    /// Announce that we're downloading a torrent.
    ///
    /// Finds the closest nodes to the infohash in our routing table,
    /// obtains tokens via get_peers, and announces to those nodes.
    ///
    /// Returns the number of nodes we're attempting to announce to.
    /// Call `get_peers` separately to discover peers you can connect to.
    pub async fn announce(&self, info_hash: InfoHash, port: u16) -> Result<usize, DhtError> {
        self.announce_peer_ext(info_hash, port, false).await
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
    ) -> Result<usize, DhtError> {
        self.announce_peer_ext(info_hash, port, implied_port).await
    }
}

// ============================================================================
// Internal: Commands
// ============================================================================

enum DhtCommand {
    Bootstrap {
        nodes: Vec<String>,
        resp: oneshot::Sender<Result<(), DhtError>>,
    },
    FindNode {
        target: NodeId,
        resp: oneshot::Sender<Result<Vec<CompactNodeInfo>, DhtError>>,
    },
    GetPeers {
        info_hash: InfoHash,
        peer_tx: mpsc::Sender<Vec<SocketAddrV4>>,
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
    /// Storage for announced peers.
    peer_store: PeerStore,
    /// Token generation and validation.
    token_manager: TokenManager,
    /// Path to store the node ID file for persistence.
    id_file_path: Option<PathBuf>,
    /// Path to store the DHT state (routing table) for persistence.
    state_file_path: Option<PathBuf>,
    transaction_manager: TransactionManager,
    /// Active get_peers lookups and their collected results
    pending_get_peers: std::collections::HashMap<InfoHash, GetPeersLookupState>,
}

/// State for an active get_peers lookup
#[derive(Debug)]
struct GetPeersLookupState {
    /// Peers discovered so far
    peers: std::collections::HashSet<SocketAddrV4>,
    /// Nodes with tokens for potential announce
    nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)>,
    /// Nodes we've queried
    queried_nodes: std::collections::HashSet<SocketAddrV4>,
    /// Channel to stream discovered peers to caller
    peer_tx: mpsc::Sender<Vec<SocketAddrV4>>,
    /// When the lookup started
    started_at: std::time::Instant,
}

impl DhtActor {
    fn new(
        socket: Arc<UdpSocket>,
        node_id: NodeId,
        command_rx: mpsc::Receiver<DhtCommand>,
        id_file_path: Option<PathBuf>,
        state_file_path: Option<PathBuf>,
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
        }
    }

    async fn run(mut self, shared_node_id: Arc<std::sync::RwLock<NodeId>>) -> Result<(), DhtError> {
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
                }

                // Handle commands from the public API
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        DhtCommand::Bootstrap { nodes, resp } => {
                            let result = self.bootstrap(&nodes).await;
                            let _ = resp.send(result);
                        }
                        DhtCommand::FindNode { target, resp } => {
                            // Perform iterative find_node lookup
                            let result = self.iterative_find_node(target).await;
                            let _ = resp.send(result);
                        }
                        DhtCommand::GetPeers { info_hash, peer_tx } => {
                            // Start get_peers lookup - fires queries and streams results
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
                                if let SocketAddr::V4(addr_v4) = tx.addr {
                                    let msg = KrpcMessage::ping_query(
                                        u16::from_be_bytes(tx.tx_id.0),
                                        self.node_id
                                    );
                                    let _ = self.socket.try_send_to(&msg.to_bytes(), SocketAddr::V4(addr_v4));
                                }
                            }
                            QueryType::FindNode { target } => {
                                if let SocketAddr::V4(addr_v4) = tx.addr {
                                    let msg = KrpcMessage::find_node_query(
                                        u16::from_be_bytes(tx.tx_id.0),
                                        self.node_id,
                                        *target
                                    );
                                    let _ = self.socket.try_send_to(&msg.to_bytes(), SocketAddr::V4(addr_v4));
                                }
                            }
                            QueryType::GetPeers { info_hash } => {
                                if let SocketAddr::V4(addr_v4) = tx.addr {
                                    let msg = KrpcMessage::get_peers_query(
                                        u16::from_be_bytes(tx.tx_id.0),
                                        self.node_id,
                                        *info_hash
                                    );
                                    let _ = self.socket.try_send_to(&msg.to_bytes(), SocketAddr::V4(addr_v4));
                                }
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
    // Bootstrap
    // ========================================================================

    async fn bootstrap(&mut self, bootstrap_nodes: &[String]) -> Result<(), DhtError> {
        tracing::info!(
            "Starting DHT bootstrap with {} nodes",
            bootstrap_nodes.len()
        );

        // Try to load persisted state for faster bootstrap
        let persisted_nodes = self.state_file_path.as_ref().and_then(DhtActor::load_state);

        if let Some((saved_id, ref saved_nodes)) = persisted_nodes {
            tracing::info!(
                "Found persisted DHT state: {} nodes, node_id={:?}",
                saved_nodes.len(),
                saved_id
            );
        }

        // Resolve bootstrap addresses
        let mut addrs: Vec<SocketAddr> = Vec::new();
        for node in bootstrap_nodes {
            match node.to_socket_addrs() {
                Ok(resolved) => addrs.extend(resolved),
                Err(e) => tracing::warn!("Failed to resolve {node}: {e}"),
            }
        }

        if addrs.is_empty() && persisted_nodes.is_none() {
            return Err(DhtError::BootstrapFailed);
        }

        // Add persisted nodes to routing table
        if let Some((saved_id, ref saved_nodes)) = persisted_nodes {
            tracing::info!(
                "Adding {} persisted nodes to routing table",
                saved_nodes.len()
            );
            for node_info in saved_nodes {
                let node = Node::new(node_info.node_id, node_info.addr);
                self.routing_table.try_add_node(node);
            }
        }

        // Send ping to bootstrap nodes - this is how we initially populate the routing table
        // The ping->pong handshake lets us learn their node IDs
        for addr in &addrs {
            let SocketAddr::V4(addr_v4) = addr else {
                continue;
            };

            self.send_ping(*addr_v4)?;
        }

        tracing::info!(
            "Bootstrap complete: sent pings to {} bootstrap nodes, {} nodes in routing table",
            addrs.len(),
            self.routing_table.node_count()
        );
        Ok(())
        //
        //     match self.ping(*addr_v4).await {
        //         Ok((msg, _)) => {
        //             if let Some(sender_ip) = msg.sender_ip {
        //                 external_ip = Some(*sender_ip.ip());
        //             }
        //
        //             if let Some(node_id) = msg.get_node_id() {
        //                 first_node = Some((node_id, *addr_v4));
        //                 break;
        //             }
        //         }
        //         Err(e) => {
        //             tracing::debug!("Bootstrap ping to {addr} failed: {e}");
        //         }
        //     }
        // }
        //
        // // Check/update BEP 42 secure node ID
        // if let Some(ip) = external_ip {
        //     let ip_addr = IpAddr::V4(ip);
        //
        //     if self.node_id.is_node_id_secure(ip_addr) {
        //         tracing::info!(
        //             "Reusing existing BEP 42 compliant node ID: {:?}",
        //             self.node_id
        //         );
        //     } else {
        //         let mut secure_id = NodeId::generate_random();
        //         secure_id.secure_node_id(&ip_addr);
        //         self.node_id = secure_id;
        //         self.routing_table = RoutingTable::new(secure_id);
        //         tracing::info!("Generated new BEP 42 secure node ID: {:?}", self.node_id);
        //
        //         if let Some(ref path) = self.id_file_path {
        //             if let Err(e) = self.node_id.save(path) {
        //                 tracing::warn!("Failed to save node ID to {}: {}", path.display(), e);
        //             } else {
        //                 tracing::info!("Saved new BEP 42 node ID to {}", path.display());
        //             }
        //         }
        //     }
        // } else {
        //     tracing::warn!("Could not discover external IP, using random node ID");
        // }
        //
        // // Add the first responding bootstrap node
        // if let Some((node_id, addr)) = first_node {
        //     let node = Node::new_good(node_id, addr);
        //     self.routing_table.try_add_node(node);
        //     tracing::info!("Added bootstrap node to routing table");
        // }
        //
        // // Ping persisted nodes and add responsive ones to the routing table
        // if let Some((_, saved_nodes)) = persisted_nodes {
        //     let total = saved_nodes.len();
        //     let mut added = 0usize;
        //
        //     for node_info in &saved_nodes {
        //         match self.ping(node_info.addr).await {
        //             Ok((msg, _)) => {
        //                 if let Some(resp_id) = msg.get_node_id() {
        //                     let node = Node::new_good(resp_id, node_info.addr);
        //                     self.routing_table.try_add_node(node);
        //                     added += 1;
        //                 }
        //             }
        //             Err(_) => {
        //                 tracing::debug!("Persisted node {} did not respond", node_info.addr);
        //             }
        //         }
        //     }
        //
        //     tracing::info!(
        //         "Pinged {} persisted nodes, {} responded and added to routing table",
        //         total,
        //         added
        //     );
        // }
        //
        // // Perform iterative find_node on ourselves to populate routing table
        // tracing::info!("Starting iterative find_node to populate routing table...");
        // // TODO: Perfomr this async
        //
        // let node_count = self.routing_table.node_count();
        // if node_count == 0 {
        //     return Err(DhtError::BootstrapFailed);
        // }
        //
        // tracing::info!("Bootstrap complete: {} nodes in routing table", node_count);
        // Ok(self.node_id)
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

        let mut queried_nodes: HashSet<SocketAddrV4> = HashSet::new();
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
        peer_tx: mpsc::Sender<Vec<SocketAddrV4>>,
    ) {
        use std::collections::HashSet;

        let target = NodeId::from(info_hash);
        let mut queried_nodes: HashSet<SocketAddrV4> = HashSet::new();

        let lookup_state = GetPeersLookupState {
            peers: HashSet::new(),
            nodes_with_tokens: Vec::new(),
            queried_nodes: HashSet::new(),
            peer_tx,
            started_at: std::time::Instant::now(),
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

            for node_addr in DEFAULT_BOOTSTRAP_NODES.iter() {
                if let Ok(addrs) = node_addr.to_socket_addrs() {
                    for addr in addrs {
                        if let SocketAddr::V4(addr_v4) = addr {
                            if !queried_nodes.contains(&addr_v4) {
                                queried_nodes.insert(addr_v4);
                                let _ = self.send_get_peers(addr_v4, info_hash);
                            }
                        }
                    }
                }
            }
        } else {
            for node_info in closest_nodes.iter().take(3) {
                if !queried_nodes.contains(&node_info.addr) {
                    queried_nodes.insert(node_info.addr);
                    let _ = self.send_get_peers(node_info.addr, info_hash);
                }
            }
        }
    }

    // ========================================================================
    // Announce
    // ========================================================================

    /// Send get_peers query with context to trigger announce after receiving token.
    fn send_get_peers_with_announce(
        &mut self,
        addr: SocketAddrV4,
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    ) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("get_peers", &SocketAddr::V4(addr))
            .is_some()
        {
            return Ok(());
        }

        // Create transaction with announce context
        let mut tx = Transaction::new(
            tx_id.clone(),
            SocketAddr::V4(addr),
            QueryType::GetPeers { info_hash },
        );
        tx.announce_port = Some(port);
        tx.implied_port = implied_port;

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg =
            KrpcMessage::get_peers_query(u16::from_be_bytes(tx_id.0), self.node_id, info_hash);
        let bytes = msg.to_bytes();

        match self.socket.try_send_to(&bytes, SocketAddr::V4(addr)) {
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
    fn send_ping(&mut self, addr: SocketAddrV4) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("ping", &SocketAddr::V4(addr))
            .is_some()
        {
            return Ok(());
        }

        // Create and register transaction
        let tx = Transaction::new(tx_id.clone(), SocketAddr::V4(addr), QueryType::Ping);

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg = KrpcMessage::ping_query(u16::from_be_bytes(tx_id.0), self.node_id);
        let bytes = msg.to_bytes();

        match self.socket.try_send_to(&bytes, SocketAddr::V4(addr)) {
            Ok(_) => Ok(()),
            Err(e) => {
                // Remove transaction if send failed
                self.transaction_manager.finish_by_trans_id(&tx_id);
                Err(DhtError::Network(e))
            }
        }
    }

    /// Send a find_node query without awaiting response.
    fn send_find_node(&mut self, addr: SocketAddrV4, target: NodeId) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("find_node", &SocketAddr::V4(addr))
            .is_some()
        {
            return Ok(());
        }

        // Create and register transaction
        let tx = Transaction::new(
            tx_id.clone(),
            SocketAddr::V4(addr),
            QueryType::FindNode { target },
        );

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg = KrpcMessage::find_node_query(u16::from_be_bytes(tx_id.0), self.node_id, target);
        let bytes = msg.to_bytes();

        match self.socket.try_send_to(&bytes, SocketAddr::V4(addr)) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.transaction_manager.finish_by_trans_id(&tx_id);
                Err(DhtError::Network(e))
            }
        }
    }

    /// Send a get_peers query without awaiting response.
    fn send_get_peers(&mut self, addr: SocketAddrV4, info_hash: InfoHash) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Check for duplicate
        if self
            .transaction_manager
            .get_by_index("get_peers", &SocketAddr::V4(addr))
            .is_some()
        {
            return Ok(());
        }

        // Create and register transaction
        let tx = Transaction::new(
            tx_id.clone(),
            SocketAddr::V4(addr),
            QueryType::GetPeers { info_hash },
        );

        if !self.transaction_manager.insert(tx) {
            return Ok(()); // Already exists
        }

        // Build and send query
        let msg =
            KrpcMessage::get_peers_query(u16::from_be_bytes(tx_id.0), self.node_id, info_hash);
        let bytes = msg.to_bytes();

        match self.socket.try_send_to(&bytes, SocketAddr::V4(addr)) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.transaction_manager.finish_by_trans_id(&tx_id);
                Err(DhtError::Network(e))
            }
        }
    }

    /// Send an announce_peer query without awaiting response.
    fn send_announce_peer(
        &mut self,
        addr: SocketAddrV4,
        info_hash: InfoHash,
        port: u16,
        token: Vec<u8>,
        implied_port: bool,
    ) -> Result<(), DhtError> {
        let tx_id = self.transaction_manager.gen_id();

        // Create and register transaction (no duplicate check needed for announce)
        let tx = Transaction::new(
            tx_id.clone(),
            SocketAddr::V4(addr),
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
        let bytes = msg.to_bytes();

        match self.socket.try_send_to(&bytes, SocketAddr::V4(addr)) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.transaction_manager.finish_by_trans_id(&tx_id);
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
                                // Update routing table
                                if let SocketAddr::V4(addr_v4) = from {
                                    let node = Node::new_good(node_id, addr_v4);
                                    self.routing_table.try_add_node(node);
                                }
                            }
                        }
                        QueryType::FindNode { target } => {
                            if let Response::FindNode { nodes, .. } = response {
                                for node_info in nodes {
                                    // Add to routing table
                                    let new_node = Node::new(node_info.node_id, node_info.addr);
                                    self.routing_table.try_add_node(new_node);

                                    // Continue searching toward target
                                    // Only if we haven't already queried this node
                                    if self
                                        .transaction_manager
                                        .get_by_index("find_node", &SocketAddr::V4(node_info.addr))
                                        .is_none()
                                    {
                                        let _ = self.send_find_node(node_info.addr, *target);
                                    }
                                }
                            }
                        }
                        QueryType::GetPeers { info_hash } => {
                            match response {
                                Response::GetPeers {
                                    values,
                                    nodes,
                                    token,
                                    ..
                                } => {
                                    // Get the sender's node info from the transaction
                                    let sender_node_info = if let SocketAddr::V4(from_v4) = from {
                                        // Try to get node ID from response or transaction
                                        let node_id = msg.get_node_id().unwrap_or(self.node_id);
                                        Some(CompactNodeInfo {
                                            node_id,
                                            addr: from_v4,
                                        })
                                    } else {
                                        None
                                    };

                                    // Update pending lookup state and stream peers to caller
                                    if let Some(state) = self.pending_get_peers.get_mut(info_hash) {
                                        if let Some(peers) = values {
                                            let peer_vec: Vec<SocketAddrV4> = peers
                                                .iter()
                                                .filter(|p| state.peers.insert(**p))
                                                .map(|p| *p)
                                                .collect();
                                            
                                            if !peer_vec.is_empty() {
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
                                            let new_node =
                                                Node::new(node_info.node_id, node_info.addr);
                                            self.routing_table.try_add_node(new_node);

                                            // Send get_peers to this node
                                            if self
                                                .transaction_manager
                                                .get_by_index(
                                                    "get_peers",
                                                    &SocketAddr::V4(node_info.addr),
                                                )
                                                .is_none()
                                            {
                                                let _ =
                                                    self.send_get_peers(node_info.addr, *info_hash);
                                            }
                                        }
                                    }

                                    // If we have an announce_port set, send announce_peer
                                    if let Some(port) = tx.announce_port {
                                        if let SocketAddr::V4(from_v4) = from {
                                            let _ = self.send_announce_peer(
                                                from_v4,
                                                *info_hash,
                                                port,
                                                token.clone(),
                                                tx.implied_port,
                                            );
                                        }
                                    }
                                }
                                _ => {}
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
        let SocketAddr::V4(addr_v4) = addr else {
            return Err(DhtError::Network(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "IPv6 not supported",
            )));
        };

        tracing::debug!("Attempting to add node {} to routing table", addr);

        self.send_ping(addr_v4)
    }

    // ========================================================================
    // DHT Maintenance
    // ========================================================================

    /// Perform periodic DHT maintenance tasks:
    /// 1. Bucket Maintenance: Trigger find_node for stale buckets
    /// 2. Neighborhood Maintenance: Aggressive for our own bucket
    /// 3. Proactive Recovery: Fill empty buckets, random recovery
    fn perform_maintenance(&mut self) {
        use rand::Rng;
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
        if rng.random_range(0..8) == 0 {
            if let Some(bucket_index) = (0..160).find(|_| true) {
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
}
