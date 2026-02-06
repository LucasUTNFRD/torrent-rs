//! Mock implementations for testing
//!
//! Provides mock implementations of external dependencies similar to
//! libtransmission's MockDht, MockMediator, etc.

use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::sync::{Arc, Mutex};

use bittorrent_common::types::InfoHash;
use mainline_dht::{AnnounceResult, CompactNodeInfo, GetPeersResult, NodeId};

/// Mock DHT implementation for testing
///
/// Similar to libtransmission's MockDht class.
/// Tracks all method calls and allows simulating responses.
#[derive(Debug, Clone)]
pub struct MockDht {
    inner: Arc<Mutex<MockDhtInner>>,
}

#[derive(Debug)]
struct MockDhtInner {
    /// Number of "good" nodes in the mock routing table
    good_nodes: usize,
    /// Number of "dubious" nodes
    dubious_nodes: usize,
    /// Number of cached nodes
    cached_nodes: usize,
    /// Number of incoming connections
    incoming_count: usize,
    /// Nodes that have been pinged
    pinged_nodes: Vec<SocketAddrV4>,
    /// Searches that have been performed
    searches: Vec<(InfoHash, u16)>, // (info_hash, port)
    /// Announces that have been made
    announces: Vec<(InfoHash, u16)>, // (info_hash, port)
    /// Whether the DHT is initialized
    initialized: bool,
    /// Bootstrap nodes that would be used
    bootstrap_nodes: Vec<SocketAddrV4>,
}

impl MockDht {
    /// Create a new mock DHT
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockDhtInner {
                good_nodes: 0,
                dubious_nodes: 0,
                cached_nodes: 0,
                incoming_count: 0,
                pinged_nodes: Vec::new(),
                searches: Vec::new(),
                announces: Vec::new(),
                initialized: false,
                bootstrap_nodes: Vec::new(),
            })),
        }
    }

    /// Set the number of good nodes (simulates healthy swarm)
    pub fn set_healthy_swarm(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.good_nodes = 50;
        inner.incoming_count = 10;
    }

    /// Set the number of good nodes to simulate firewalled scenario
    pub fn set_firewalled_swarm(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.good_nodes = 50;
        inner.incoming_count = 0;
    }

    /// Set poor swarm conditions
    pub fn set_poor_swarm(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.good_nodes = 10;
        inner.incoming_count = 1;
    }

    /// Record a ping to a node
    pub fn record_ping(&self, addr: SocketAddrV4) {
        let mut inner = self.inner.lock().unwrap();
        inner.pinged_nodes.push(addr);
    }

    /// Get all nodes that have been pinged
    pub fn get_pinged_nodes(&self) -> Vec<SocketAddrV4> {
        let inner = self.inner.lock().unwrap();
        inner.pinged_nodes.clone()
    }

    /// Record a search
    pub fn record_search(&self, info_hash: InfoHash, port: u16) {
        let mut inner = self.inner.lock().unwrap();
        inner.searches.push((info_hash, port));
    }

    /// Get all searches performed
    pub fn get_searches(&self) -> Vec<(InfoHash, u16)> {
        let inner = self.inner.lock().unwrap();
        inner.searches.clone()
    }

    /// Record an announce
    pub fn record_announce(&self, info_hash: InfoHash, port: u16) {
        let mut inner = self.inner.lock().unwrap();
        inner.announces.push((info_hash, port));
    }

    /// Get all announces made
    pub fn get_announces(&self) -> Vec<(InfoHash, u16)> {
        let inner = self.inner.lock().unwrap();
        inner.announces.clone()
    }

    /// Set bootstrap nodes
    pub fn set_bootstrap_nodes(&self, nodes: Vec<SocketAddrV4>) {
        let mut inner = self.inner.lock().unwrap();
        inner.bootstrap_nodes = nodes;
    }

    /// Get bootstrap nodes
    pub fn get_bootstrap_nodes(&self) -> Vec<SocketAddrV4> {
        let inner = self.inner.lock().unwrap();
        inner.bootstrap_nodes.clone()
    }

    /// Mark as initialized
    pub fn set_initialized(&self, initialized: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.initialized = initialized;
    }

    /// Check if initialized
    pub fn is_initialized(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.initialized
    }

    /// Get swarm health metrics
    pub fn get_swarm_metrics(&self) -> SwarmMetrics {
        let inner = self.inner.lock().unwrap();
        SwarmMetrics {
            good_nodes: inner.good_nodes,
            dubious_nodes: inner.dubious_nodes,
            cached_nodes: inner.cached_nodes,
            incoming: inner.incoming_count,
        }
    }

    /// Create a mock GetPeersResult
    pub fn mock_get_peers_result(&self, peers: Vec<SocketAddrV4>) -> GetPeersResult {
        GetPeersResult {
            peers,
            nodes_contacted: 10,
            nodes_with_tokens: Vec::new(),
        }
    }

    /// Create a mock AnnounceResult
    pub fn mock_announce_result(&self, peers: Vec<SocketAddrV4>) -> AnnounceResult {
        AnnounceResult {
            peers,
            announce_count: 8,
        }
    }

    /// Reset all recorded data
    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.pinged_nodes.clear();
        inner.searches.clear();
        inner.announces.clear();
        inner.good_nodes = 0;
        inner.incoming_count = 0;
    }
}

impl Default for MockDht {
    fn default() -> Self {
        Self::new()
    }
}

/// Swarm health metrics
#[derive(Debug, Clone, Copy)]
pub struct SwarmMetrics {
    pub good_nodes: usize,
    pub dubious_nodes: usize,
    pub cached_nodes: usize,
    pub incoming: usize,
}

impl SwarmMetrics {
    /// Check if swarm is healthy (has enough good nodes and incoming connections)
    pub fn is_healthy(&self) -> bool {
        self.good_nodes >= 30 && self.incoming > 0
    }
}

/// Mock DHT state file (like libtransmission's dht.dat)
#[derive(Debug, Clone)]
pub struct DhtState {
    pub id: NodeId,
    pub id_timestamp: u64,
    pub nodes: Vec<CompactNodeInfo>,
    pub nodes6: Vec<CompactNodeInfo>, // IPv6 nodes
}

impl DhtState {
    /// Create a new DHT state with random ID
    pub fn new() -> Self {
        Self {
            id: NodeId::generate_random(),
            id_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            nodes: Vec::new(),
            nodes6: Vec::new(),
        }
    }

    /// Add bootstrap nodes
    pub fn with_nodes(mut self, nodes: Vec<CompactNodeInfo>) -> Self {
        self.nodes = nodes;
        self
    }

    /// Save state to file (bencoded)
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), std::io::Error> {
        // TODO: Implement bencode serialization
        // For now, just create an empty file as placeholder
        std::fs::write(path, b"")?;
        Ok(())
    }

    /// Load state from file
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, std::io::Error> {
        // TODO: Implement bencode deserialization
        let _data = std::fs::read(path)?;
        Ok(Self::new())
    }
}

impl Default for DhtState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn mock_dht_tracks_pings() {
        let mock = MockDht::new();

        let addr = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 6881);
        mock.record_ping(addr);

        let pinged = mock.get_pinged_nodes();
        assert_eq!(pinged.len(), 1);
        assert_eq!(pinged[0], addr);
    }

    #[test]
    fn mock_dht_tracks_searches() {
        let mock = MockDht::new();

        let info_hash = InfoHash::new(b"12345678901234567890");
        mock.record_search(info_hash, 6881);

        let searches = mock.get_searches();
        assert_eq!(searches.len(), 1);
        assert_eq!(searches[0].0, info_hash);
        assert_eq!(searches[0].1, 6881);
    }

    #[test]
    fn swarm_metrics_health_check() {
        let healthy = SwarmMetrics {
            good_nodes: 50,
            dubious_nodes: 10,
            cached_nodes: 100,
            incoming: 5,
        };
        assert!(healthy.is_healthy());

        let firewalled = SwarmMetrics {
            good_nodes: 50,
            incoming: 0,
            ..healthy
        };
        assert!(!firewalled.is_healthy());

        let poor = SwarmMetrics {
            good_nodes: 5,
            incoming: 1,
            ..healthy
        };
        assert!(!poor.is_healthy());
    }
}
