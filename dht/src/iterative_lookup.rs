//! Generic iterative Kademlia lookup driver.
//!
//! The three DHT lookup flavors — bootstrap, `get_peers` search, and bucket
//! refresh — all follow the same iterative pattern from BEP 5:
//!
//!  1. Seed a set of candidate nodes sorted by XOR distance to a target.
//!  2. Query the closest unqueried candidates (up to α at a time).
//!  3. Process responses: collect results, insert newly-discovered nodes.
//!  4. Repeat until the K closest nodes have all responded (or all candidates
//!     are exhausted).
//!
//! This module extracts that common loop into [`IterativeLookup`], parameterized
//! by a [`LookupBehavior`] trait that controls only the parts that differ:
//!
//! - [`FindNodeBehavior`]: Used by **bootstrap** (target = our own ID, 30 s
//!   deadline, addr-only seeds from DNS) and **bucket refresh** (target =
//!   random ID in bucket range, no deadline).
//! - [`GetPeersBehavior`]: Used by **`get_peers` search** (collects peers,
//!   stores tokens, optional `announce_peer` post-phase).

use crate::{
    dht::DhtNodeCommand,
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

/// Kademlia concurrency parameter: maximum in-flight queries at a time.
const ALPHA: usize = 5;

// ============================================================================
// Candidate Tracking
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeState {
    Unqueried,
    InFlight,
    Responded,
    Failed,
}

#[derive(Clone)]
struct CandidateNode {
    /// `None` for bootstrap seeds where we only have a `SocketAddr` from DNS.
    id: Option<NodeId>,
    addr: SocketAddr,
    state: NodeState,
    token: Option<Vec<u8>>,
}

/// XOR distance from target, used as the `BTreeMap` key so that iteration
/// order is automatically closest-first.
type DistanceToTarget = NodeId;

/// Tracks candidate nodes during an iterative lookup, sorted by XOR
/// distance to the target.
///
/// The `BTreeMap` key is `target ^ node_id`, so iteration visits the
/// closest candidates first without explicit sorting.
struct ClosestNodes {
    target: NodeId,
    nodes: BTreeMap<DistanceToTarget, CandidateNode>,
    /// Deduplicates by `SocketAddr` across all insert calls
    /// (including bootstrap seeds and discovered nodes).
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

    /// Insert a node at its correct XOR-distance position.
    /// Duplicate addresses are silently ignored.
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

    /// Pop up to `count` of the closest unqueried candidates and mark
    /// them as in-flight.
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

    /// Look up the `NodeId` for a candidate we're about to query.
    /// Returns `None` for bootstrap seeds that haven't responded yet.
    fn get_node_id_for_addr(&self, addr: SocketAddr) -> Option<NodeId> {
        self.nodes
            .values()
            .find(|n| n.addr == addr)
            .and_then(|n| n.id)
    }

    /// Check whether the iterative lookup has converged.
    ///
    /// Complete when either:
    /// - The K closest candidates have all responded (no in-flight), or
    /// - Every candidate among the top K has been processed (responded
    ///   or failed), which handles the case where fewer than K nodes exist.
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

        let reached_full_k = responded_count >= K && in_flight_count == 0;

        let is_processed =
            |n: &&CandidateNode| n.state == NodeState::Responded || n.state == NodeState::Failed;

        let exhausted_top_k = top_k_nodes.iter().all(is_processed);

        reached_full_k || exhausted_top_k
    }

    /// Return the K closest nodes that responded and have a token + id,
    /// for use in the `announce_peer` post-phase.
    fn get_k_closest_responded(&self) -> Vec<(SocketAddr, NodeId, Vec<u8>)> {
        self.nodes
            .values()
            .filter(|n| n.state == NodeState::Responded && n.token.is_some() && n.id.is_some())
            .take(K)
            .map(|n| {
                (
                    n.addr,
                    n.id.expect("filtered above"),
                    n.token.clone().expect("filtered above"),
                )
            })
            .collect()
    }
}

// ============================================================================
// Generic Iterative Lookup
// ============================================================================

/// Build the correct `Want` value from an address family.
const fn want_for_family(af: AddressFamily) -> Want {
    match af {
        AddressFamily::V4 => Want::v4_only(),
        AddressFamily::V6 => Want::v6_only(),
    }
}

/// Defines the variable behavior for an iterative Kademlia lookup.
///
/// The generic [`IterativeLookup`] drives the common loop (candidate
/// selection, ALPHA-bounded in-flight pipeline, completion checks).
/// Implementors control only the parts that differ: which query to
/// send, how to handle a response, and what to return.
#[allow(async_fn_in_trait)] // Only used with generics (monomorphized), not dyn dispatch.
trait LookupBehavior {
    /// The value the lookup produces when finished.
    type Output;

    /// Build the KRPC query to send to the next candidate node.
    fn build_query(&self, target_node_id: Option<NodeId>) -> Query;

    /// Process a successful response. Implementations should:
    /// - Update candidate state (responded, insert new nodes).
    /// - Collect any results (peers, tokens).
    /// - Optionally send `DhtNodeCommand::AddNode` via `dht_tx`.
    async fn on_response(
        &mut self,
        addr: SocketAddr,
        response: Response,
        candidates: &mut ClosestNodes,
        dht_tx: &mpsc::Sender<DhtNodeCommand>,
    );

    /// Called once the iterative lookup is complete. Consumes self so
    /// implementations can drain accumulated state (e.g. collected
    /// peers, candidate tokens for announce).
    async fn finish(
        self,
        candidates: ClosestNodes,
        dht_tx: &mpsc::Sender<DhtNodeCommand>,
    ) -> Self::Output;
}

/// Configuration for an iterative lookup.
struct LookupConfig {
    /// Maximum wall-clock time before the lookup is abandoned.
    /// `None` means no deadline (the default for search / refresh).
    /// Bootstrap uses a 30 s deadline to avoid hanging on startup.
    deadline: Option<tokio::time::Instant>,

    /// Extra seed addresses consumed before `ClosestNodes` candidates.
    ///
    /// Bootstrap uses this to drain DNS-resolved addresses (which
    /// have no `NodeId`) before falling through to XOR-sorted candidates.
    /// For search and refresh, this is empty.
    seed_addrs: Vec<SocketAddr>,
}

/// Drives a Kademlia iterative lookup to completion.
///
/// All three lookup flavors (bootstrap, `get_peers` search, bucket
/// refresh) use this single driver, varying only their
/// [`LookupBehavior`] implementation.
struct IterativeLookup<B: LookupBehavior> {
    behavior: B,
    dht_node_tx: mpsc::Sender<DhtNodeCommand>,
    candidates: ClosestNodes,
    config: LookupConfig,
}

impl<B: LookupBehavior> IterativeLookup<B> {
    async fn run(mut self) -> B::Output {
        let mut futs = FuturesUnordered::new();

        loop {
            // ── 1. Fill pipeline up to ALPHA ──────────────────────────
            //
            // Seed addresses (bootstrap DNS entries) are consumed first.
            // Once exhausted, pull unqueried candidates from ClosestNodes
            // (already XOR-sorted by BTreeMap key, closest first).
            while futs.len() < ALPHA {
                let addr = if let Some(addr) = self.config.seed_addrs.pop() {
                    addr
                } else {
                    match self.candidates.get_next_unqueried(1).into_iter().next() {
                        Some(addr) => addr,
                        None => break,
                    }
                };

                let node_id = self.candidates.get_node_id_for_addr(addr);
                let query = self.behavior.build_query(node_id);
                let tx = self.dht_node_tx.clone();

                futs.push(async move {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = tx
                        .send(DhtNodeCommand::QueryNode {
                            addr,
                            node_id,
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

            // ── 2. Await next response (with optional deadline) ──────
            let response = if let Some(deadline) = self.config.deadline {
                tokio::select! {
                    r = futs.next() => r,
                    () = tokio::time::sleep_until(deadline) => {
                        tracing::warn!("Iterative lookup timed out");
                        break;
                    }
                }
            } else {
                futs.next().await
            };

            // ── 3. Process response ──────────────────────────────────
            if let Some((addr, res)) = response {
                match res {
                    Ok(Ok(resp)) => {
                        self.behavior
                            .on_response(addr, resp, &mut self.candidates, &self.dht_node_tx)
                            .await;
                    }
                    _ => {
                        // Channel error or DhtError — mark the candidate
                        // as failed so it won't be re-queried.
                        self.candidates.update_state(addr, NodeState::Failed, None);
                    }
                }
            }

            // ── 4. Check termination ─────────────────────────────────
            //
            // For bootstrap: don't check is_complete() until all seed
            // addrs are drained, because seeds live outside ClosestNodes
            // and is_complete() can't account for them.
            //
            // For search / refresh: seed_addrs is empty from the start,
            // so this reduces to just is_complete().
            if self.config.seed_addrs.is_empty() && self.candidates.is_complete() {
                break;
            }
        }

        // ── 5. Post-lookup phase ─────────────────────────────────────
        self.behavior
            .finish(self.candidates, &self.dht_node_tx)
            .await
    }
}

// ============================================================================
// FindNode Behavior (Bootstrap + Refresh)
// ============================================================================

/// Behavior for `find_node` lookups: used by both bootstrap (targeting
/// our own `NodeId` to populate the routing table) and bucket refresh
/// (targeting a random ID in a stale bucket's range).
///
/// On each response, discovered nodes are sent to the DHT actor via
/// `AddNode` commands so they get inserted into the routing table
/// alongside being added to the candidate set.
struct FindNodeBehavior {
    target: NodeId,
    our_node_id: NodeId,
    family: AddressFamily,
    is_bootstrap: bool,
}

impl LookupBehavior for FindNodeBehavior {
    type Output = ();

    fn build_query(&self, _node_id: Option<NodeId>) -> Query {
        Query::FindNode {
            id: self.our_node_id,
            target: self.target,
            is_bootstrap: self.is_bootstrap,
            want: Some(want_for_family(self.family)),
        }
    }

    async fn on_response(
        &mut self,
        addr: SocketAddr,
        response: Response,
        candidates: &mut ClosestNodes,
        dht_tx: &mpsc::Sender<DhtNodeCommand>,
    ) {
        if let Response::FindNode { id, nodes } = response {
            // Now that we have the real NodeId, insert at correct XOR
            // distance. For bootstrap seeds this is the first time we
            // learn their ID; for other candidates this is a seen-dedup
            // no-op on the addr.
            candidates.insert(id, addr);
            candidates.update_state(addr, NodeState::Responded, None);

            for node in nodes {
                let _ = dht_tx
                    .send(DhtNodeCommand::AddNode {
                        id: node.node_id,
                        addr: node.addr,
                    })
                    .await;
                candidates.insert(node.node_id, node.addr);
            }
        } else {
            // Unexpected response type (e.g. we got a Ping response
            // when we sent FindNode). Treat as failure.
            candidates.update_state(addr, NodeState::Failed, None);
        }
    }

    async fn finish(self, _candidates: ClosestNodes, _dht_tx: &mpsc::Sender<DhtNodeCommand>) {
        // Bootstrap and refresh just populate the routing table via
        // AddNode commands sent during on_response. No post-phase.
    }
}

// ============================================================================
// GetPeers Behavior (Search + Announce)
// ============================================================================

/// Behavior for `get_peers` lookups: collects peer contact information
/// and, optionally, announces to the K closest responsive nodes
/// afterwards.
struct GetPeersBehavior {
    target: NodeId,
    our_node_id: NodeId,
    family: AddressFamily,
    announce_port: Option<u16>,
    discovered_peers: HashSet<SocketAddr>,
}

impl LookupBehavior for GetPeersBehavior {
    type Output = Vec<SocketAddr>;

    fn build_query(&self, _node_id: Option<NodeId>) -> Query {
        Query::GetPeers {
            id: self.our_node_id,
            info_hash: InfoHash::from_slice(self.target.as_bytes())
                .expect("NodeId is always 20 bytes"),
            is_bootstrap: false,
            want: Some(want_for_family(self.family)),
        }
    }

    async fn on_response(
        &mut self,
        addr: SocketAddr,
        response: Response,
        candidates: &mut ClosestNodes,
        _dht_tx: &mpsc::Sender<DhtNodeCommand>,
    ) {
        if let Response::GetPeers {
            values,
            nodes,
            token,
            ..
        } = response
        {
            candidates.update_state(addr, NodeState::Responded, Some(token));

            if let Some(peers) = values {
                for peer in peers {
                    self.discovered_peers.insert(peer);
                }
            }
            if let Some(nodes) = nodes {
                for node in nodes {
                    candidates.insert(node.node_id, node.addr);
                }
            }
        } else {
            candidates.update_state(addr, NodeState::Failed, None);
        }
    }

    async fn finish(
        self,
        candidates: ClosestNodes,
        dht_tx: &mpsc::Sender<DhtNodeCommand>,
    ) -> Vec<SocketAddr> {
        if let Some(port) = self.announce_port {
            announce_to_closest(&candidates, dht_tx, self.our_node_id, self.target, port).await;
        }

        self.discovered_peers.into_iter().collect()
    }
}

/// Best-effort `announce_peer` to the K closest responsive nodes
/// discovered during the `get_peers` phase.
async fn announce_to_closest(
    candidates: &ClosestNodes,
    dht_tx: &mpsc::Sender<DhtNodeCommand>,
    our_node_id: NodeId,
    target: NodeId,
    port: u16,
) {
    let closest = candidates.get_k_closest_responded();
    let mut announce_futs = Vec::with_capacity(closest.len());

    for (addr, node_id, token) in closest {
        let tx = dht_tx.clone();
        let query = Query::AnnouncePeer {
            id: our_node_id,
            info_hash: InfoHash::from_slice(target.as_bytes()).expect("NodeId is always 20 bytes"),
            port,
            token,
            implied_port: port == 0,
        };

        announce_futs.push(async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = tx
                .send(DhtNodeCommand::QueryNode {
                    addr,
                    node_id: Some(node_id),
                    query,
                    reply: reply_tx,
                })
                .await;
            (addr, reply_rx.await)
        });
    }

    let results = join_all(announce_futs).await;
    for (addr, res) in results {
        if let Ok(Ok(Response::AnnouncePeer { .. })) = res {
            tracing::info!(target: "dht_search", %addr, "Successfully announced peer to node");
        } else {
            tracing::warn!(target: "dht_search", %addr, "Received unexpected response variant for AnnouncePeer");
        }
    }
}

// ============================================================================
// Public Entry Points
// ============================================================================

/// Start a bootstrap lookup: issue `find_node` queries targeting our own
/// `NodeId` to discover the closest nodes in the network and populate the
/// routing table.
///
/// Bootstrap seeds (DNS-resolved addresses with no known `NodeId`) are
/// queried first, then the iterative lookup continues with discovered
/// candidates. A 30 s deadline prevents hanging on unresponsive seeds.
pub fn start_bootstrap(
    bootstrap_addrs: Vec<SocketAddr>,
    our_node_id: NodeId,
    dht_node_tx: mpsc::Sender<DhtNodeCommand>,
    family: AddressFamily,
) -> JoinHandle<()> {
    let behavior = FindNodeBehavior {
        target: our_node_id,
        our_node_id,
        family,
        is_bootstrap: true,
    };

    let lookup = IterativeLookup {
        behavior,
        dht_node_tx,
        candidates: ClosestNodes::new(our_node_id),
        config: LookupConfig {
            deadline: Some(tokio::time::Instant::now() + Duration::from_secs(30)),
            seed_addrs: bootstrap_addrs,
        },
    };

    tokio::spawn(async move {
        lookup.run().await;
    })
}

/// Start a `get_peers` search: iteratively query nodes for peers
/// associated with the target `InfoHash`, optionally followed by
/// an `announce_peer` phase.
pub fn start_search(
    target: NodeId,
    our_node_id: NodeId,
    dht_node_tx: mpsc::Sender<DhtNodeCommand>,
    announce_port: Option<u16>,
    af: AddressFamily,
    initial_nodes: &[NodeEntry],
) -> JoinHandle<Vec<SocketAddr>> {
    let mut candidates = ClosestNodes::new(target);
    for node in initial_nodes {
        candidates.insert(node.node_id, node.addr);
    }

    let behavior = GetPeersBehavior {
        target,
        our_node_id,
        family: af,
        announce_port,
        discovered_peers: HashSet::default(),
    };

    let lookup = IterativeLookup {
        behavior,
        dht_node_tx,
        candidates,
        config: LookupConfig {
            deadline: None,
            seed_addrs: Vec::new(),
        },
    };

    tokio::spawn(async move { lookup.run().await })
}

/// Start a bucket refresh: issue `find_node` queries targeting a random
/// ID in the stale bucket's range to discover and verify nodes.
pub fn start_refresh(
    target: NodeId,
    our_node_id: NodeId,
    dht_node_tx: mpsc::Sender<DhtNodeCommand>,
    family: AddressFamily,
    initial_nodes: &[NodeEntry],
) -> JoinHandle<()> {
    let mut candidates = ClosestNodes::new(target);
    for node in initial_nodes {
        candidates.insert(node.node_id, node.addr);
    }

    let behavior = FindNodeBehavior {
        target,
        our_node_id,
        family,
        is_bootstrap: false,
    };

    let lookup = IterativeLookup {
        behavior,
        dht_node_tx,
        candidates,
        config: LookupConfig {
            deadline: None,
            seed_addrs: Vec::new(),
        },
    };

    tokio::spawn(async move {
        lookup.run().await;
    })
}
