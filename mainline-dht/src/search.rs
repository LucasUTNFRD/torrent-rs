//! Search state management for concurrent DHT queries.
//!
//! This module implements parallel query execution for DHT searches,
//! It supports up to 4 concurrent in-flight queries per search (ALPHA) and
//! completes searches when 8 nodes have successfully responded.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    net::SocketAddrV4,
    time::Instant,
};

use bittorrent_common::types::InfoHash;
use tokio::sync::oneshot;

use crate::{
    dht::GetPeersResult,
    message::{CompactNodeInfo, TransactionId},
    node_id::NodeId,
};

// ============================================================================
// Constants
// ============================================================================

/// Maximum in-flight queries per search
pub const MAX_INFLIGHT_QUERIES: usize = 4;

/// Target number of responses to complete search.
pub const TARGET_RESPONSES: usize = 8;

/// Query timeout duration in seconds.
pub const QUERY_TIMEOUT_SECS: u64 = 2;

// ============================================================================
// Search Types
// ============================================================================

/// Status of a candidate node in a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateStatus {
    /// Not yet queried.
    Pending,
    /// Query sent, waiting for response (stores transaction ID bytes).
    Querying(TransactionId),
    /// Successfully responded.
    Responded,
    /// Timed out or error.
    Failed,
}

/// Individual candidate node in a search.
#[derive(Debug, Clone)]
pub struct SearchCandidate {
    /// Node information.
    pub node: CompactNodeInfo,
    /// XOR distance to target (for ordering).
    pub distance: NodeId,
    /// Current status.
    pub status: CandidateStatus,
    /// When the query was sent (for timeout tracking).
    pub query_sent_at: Option<Instant>,
}

impl SearchCandidate {
    /// Create a new pending candidate.
    pub fn new(node: CompactNodeInfo, target: &NodeId) -> Self {
        let distance = node.node_id ^ *target;
        Self {
            node,
            distance,
            status: CandidateStatus::Pending,
            query_sent_at: None,
        }
    }
}

// Implement ordering for SearchCandidate (by XOR distance, ascending).
// Smaller distance = higher priority (closer to target).
impl PartialEq for SearchCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.node.node_id == other.node.node_id
    }
}

impl Eq for SearchCandidate {}

impl PartialOrd for SearchCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SearchCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare by distance (smaller distance = "less than" = higher priority)
        self.distance.as_bytes().cmp(&other.distance.as_bytes())
    }
}

/// Tracks an active search for peers
pub struct SearchState {
    /// Target infohash we're searching for.
    pub info_hash: InfoHash,
    /// Target as NodeId for distance calculations.
    pub target: NodeId,
    /// Candidate nodes to query, sorted by XOR distance.
    /// We use a Vec and keep it sorted rather than BinaryHeap for easier iteration.
    pub candidates: Vec<SearchCandidate>,
    /// Number of queries currently in-flight (max 3).
    pub in_flight: usize,
    /// Number of nodes that have successfully responded (stop at 8).
    pub responded: usize,
    /// Unique peers found (deduplicated via HashSet).
    pub peers_found: HashSet<SocketAddrV4>,
    /// Nodes that responded with tokens (for announce).
    pub nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)>,
    /// Channel to send final result.
    pub completion_tx: Option<oneshot::Sender<GetPeersResult>>,
    /// When search started.
    pub started_at: Instant,
}

impl SearchState {
    /// Create a new search state.
    pub fn new(
        info_hash: InfoHash,
        initial_candidates: Vec<CompactNodeInfo>,
        completion_tx: oneshot::Sender<GetPeersResult>,
    ) -> Self {
        let target = NodeId::from(&info_hash);

        // Create candidates with distances
        let mut candidates: Vec<SearchCandidate> = initial_candidates
            .into_iter()
            .map(|node| SearchCandidate::new(node, &target))
            .collect();

        // Sort by distance (closest first)
        candidates.sort();

        Self {
            info_hash,
            target,
            candidates,
            in_flight: 0,
            responded: 0,
            peers_found: HashSet::new(),
            nodes_with_tokens: Vec::new(),
            completion_tx: Some(completion_tx),
            started_at: Instant::now(),
        }
    }

    /// Add new candidate nodes (from responses), avoiding duplicates.
    pub fn add_candidates(&mut self, nodes: Vec<CompactNodeInfo>) {
        for node in nodes {
            // Check if we already know about this node
            let already_known = self
                .candidates
                .iter()
                .any(|c| c.node.node_id == node.node_id);
            if !already_known {
                let candidate = SearchCandidate::new(node, &self.target);
                self.candidates.push(candidate);
            }
        }
        // Re-sort after adding
        self.candidates.sort();
    }

    /// Get pending candidates that can be queried (up to available slots).
    pub fn get_pending_candidates(&self, max_count: usize) -> Vec<&SearchCandidate> {
        self.candidates
            .iter()
            .filter(|c| matches!(c.status, CandidateStatus::Pending))
            .take(max_count)
            .collect()
    }

    /// Mark a candidate as querying with the given transaction ID.
    pub fn mark_querying(&mut self, node_id: &NodeId, tx_id: TransactionId) {
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|c| c.node.node_id == *node_id)
        {
            candidate.status = CandidateStatus::Querying(tx_id);
            candidate.query_sent_at = Some(Instant::now());
        }
    }

    /// Mark a candidate as responded based on transaction ID.
    pub fn mark_responded(&mut self, tx_id: &TransactionId) -> Option<NodeId> {
        for candidate in &mut self.candidates {
            if matches!(&candidate.status, CandidateStatus::Querying(id) if id == tx_id) {
                candidate.status = CandidateStatus::Responded;
                return Some(candidate.node.node_id);
            }
        }
        None
    }

    /// Mark a candidate as failed based on transaction ID.
    pub fn mark_failed(&mut self, tx_id: &TransactionId) -> Option<NodeId> {
        for candidate in &mut self.candidates {
            if matches!(&candidate.status, CandidateStatus::Querying(id) if id == tx_id) {
                candidate.status = CandidateStatus::Failed;
                return Some(candidate.node.node_id);
            }
        }
        None
    }

    /// Check if the search should complete.
    /// Completes when:
    /// - We've received TARGET_RESPONSES (8) successful responses, OR
    /// - No more candidates to query and nothing in flight
    pub fn should_complete(&self) -> bool {
        if self.responded >= TARGET_RESPONSES {
            return true;
        }

        // No more work to do
        let has_pending = self
            .candidates
            .iter()
            .any(|c| matches!(c.status, CandidateStatus::Pending));
        !has_pending && self.in_flight == 0
    }

    /// Build the final result.
    pub fn into_result(self) -> (GetPeersResult, Option<oneshot::Sender<GetPeersResult>>) {
        let result = GetPeersResult {
            peers: self.peers_found.into_iter().collect(),
            nodes_contacted: self.responded,
            nodes_with_tokens: self.nodes_with_tokens,
        };
        (result, self.completion_tx)
    }
}

/// Manager for all active searches.
pub struct SearchManager {
    /// Active searches by infohash.
    pub searches: HashMap<InfoHash, SearchState>,
    /// Map transaction ID -> infohash for routing responses.
    pub tx_to_search: HashMap<TransactionId, InfoHash>,
}

impl SearchManager {
    /// Create a new search manager.
    pub fn new() -> Self {
        Self {
            searches: HashMap::new(),
            tx_to_search: HashMap::new(),
        }
    }

    /// Start a new search.
    pub fn start_search(&mut self, search: SearchState) {
        self.searches.insert(search.info_hash, search);
    }

    /// Register a transaction ID for a search.
    pub fn register_tx(&mut self, tx_id: TransactionId, info_hash: InfoHash) {
        self.tx_to_search.insert(tx_id, info_hash);
    }

    /// Look up which search a transaction belongs to.
    pub fn lookup_tx(&self, tx_id: &TransactionId) -> Option<&InfoHash> {
        self.tx_to_search.get(tx_id)
    }

    /// Remove a transaction ID mapping.
    pub fn remove_tx(&mut self, tx_id: &TransactionId) -> Option<InfoHash> {
        self.tx_to_search.remove(tx_id)
    }

    /// Get a mutable reference to a search.
    pub fn get_search_mut(&mut self, info_hash: &InfoHash) -> Option<&mut SearchState> {
        self.searches.get_mut(info_hash)
    }

    /// Remove and return a completed search.
    pub fn remove_search(&mut self, info_hash: &InfoHash) -> Option<SearchState> {
        // Clean up any remaining TX mappings for this search
        if let Some(search) = self.searches.get(info_hash) {
            for candidate in &search.candidates {
                if let CandidateStatus::Querying(tx_id) = &candidate.status {
                    self.tx_to_search.remove(tx_id);
                }
            }
        }
        self.searches.remove(info_hash)
    }

    /// Get all active search infohashes (for timeout checking).
    pub fn active_searches(&self) -> Vec<InfoHash> {
        self.searches.keys().copied().collect()
    }
}

impl Default for SearchManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::*;

    fn make_node(id_suffix: u8) -> CompactNodeInfo {
        let mut id_bytes = [0u8; 20];
        id_bytes[19] = id_suffix;
        CompactNodeInfo {
            node_id: NodeId::from_bytes(id_bytes),
            addr: SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6881 + id_suffix as u16),
        }
    }

    #[test]
    fn test_candidate_ordering() {
        let target = NodeId::from_bytes([0u8; 20]);

        // Node with id=1 is closer to target=0 than node with id=10
        let node1 = make_node(1);
        let node10 = make_node(10);

        let candidate1 = SearchCandidate::new(node1, &target);
        let candidate10 = SearchCandidate::new(node10, &target);

        // candidate1 should be "less than" candidate10 (closer = higher priority)
        assert!(candidate1 < candidate10);
    }

    #[test]
    fn test_search_state_creation() {
        // Use target 0x00... so that XOR distance equals node ID
        let info_hash = InfoHash::new([0u8; 20]);
        let nodes = vec![make_node(1), make_node(5), make_node(2)];
        let (tx, _rx) = oneshot::channel();

        let search = SearchState::new(info_hash, nodes, tx);

        assert_eq!(search.candidates.len(), 3);
        assert_eq!(search.in_flight, 0);
        assert_eq!(search.responded, 0);
        // Should be sorted by distance (with target=0, distance = node_id)
        assert_eq!(search.candidates[0].node.node_id.as_bytes()[19], 1);
        assert_eq!(search.candidates[1].node.node_id.as_bytes()[19], 2);
        assert_eq!(search.candidates[2].node.node_id.as_bytes()[19], 5);
    }

    #[test]
    fn test_add_candidates_deduplication() {
        let info_hash = InfoHash::new([0u8; 20]);
        let nodes = vec![make_node(1), make_node(2)];
        let (tx, _rx) = oneshot::channel();

        let mut search = SearchState::new(info_hash, nodes, tx);
        assert_eq!(search.candidates.len(), 2);

        // Add a duplicate and a new node
        search.add_candidates(vec![make_node(1), make_node(3)]);

        assert_eq!(search.candidates.len(), 3);
    }

    #[test]
    fn test_should_complete() {
        let info_hash = InfoHash::new([0u8; 20]);
        let (tx, _rx) = oneshot::channel();

        let mut search = SearchState::new(info_hash, vec![], tx);

        // No candidates and nothing in flight -> should complete
        assert!(search.should_complete());

        // Add pending candidate -> should not complete
        search.add_candidates(vec![make_node(1)]);
        assert!(!search.should_complete());

        // Mark as responded 8 times -> should complete
        search.candidates.clear();
        search.responded = TARGET_RESPONSES;
        assert!(search.should_complete());
    }
}
