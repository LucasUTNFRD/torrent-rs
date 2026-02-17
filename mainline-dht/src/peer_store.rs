//! Peer storage for announced torrents using LRU eviction.
//!
//! Stores peers that have announced themselves for specific infohashes.
//! Uses an LRU cache at the torrent level and per-torrent peer limits.

use std::{
    collections::HashMap,
    net::SocketAddrV4,
    num::NonZeroUsize,
    time::{Duration, Instant},
};

use bittorrent_common::types::InfoHash;
use lru::LruCache;

/// Default maximum number of torrents to track.
const DEFAULT_MAX_TORRENTS: usize = 16384;

/// Default maximum peers stored per torrent.
const DEFAULT_MAX_PEERS: usize = 2048;

/// Default peer TTL (30 minutes per BEP 5 recommendation).
const DEFAULT_PEER_TTL: Duration = Duration::from_secs(30 * 60);

/// Stores peers for announced torrents with LRU eviction.
///
/// - Outer LRU cache limits the number of tracked torrents
/// - Each torrent has a limited set of peers with timestamps
/// - Expired peers are cleaned up on access
pub struct PeerStore {
    /// LRU cache: InfoHash -> set of peers with timestamps
    store: LruCache<InfoHash, PeerSet>,
    /// Maximum peers stored per torrent
    max_peers: usize,
    /// TTL for each peer entry
    peer_ttl: Duration,
}

/// Set of peers for a single torrent with their announce timestamps.
struct PeerSet {
    /// Peers mapped to their last announce timestamp
    peers: HashMap<SocketAddrV4, Instant>,
}

impl PeerStore {
    /// Create a new peer store with default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_TORRENTS, DEFAULT_MAX_PEERS)
    }

    /// Create a new peer store with custom capacity.
    ///
    /// # Arguments
    /// * `max_torrents` - Maximum number of infohashes to track
    /// * `max_peers` - Maximum peers stored per infohash
    pub fn with_capacity(max_torrents: usize, max_peers: usize) -> Self {
        Self {
            store: LruCache::new(NonZeroUsize::new(max_torrents).unwrap()),
            max_peers,
            peer_ttl: DEFAULT_PEER_TTL,
        }
    }

    /// Add a peer for an infohash.
    ///
    /// If the peer already exists, updates its timestamp.
    /// If the torrent's peer set is full, removes the oldest peer.
    pub fn add_peer(&mut self, info_hash: InfoHash, peer: SocketAddrV4) {
        let max_peers = self.max_peers;
        let peer_ttl = self.peer_ttl;

        let peer_set = self.store.get_or_insert_mut(info_hash, || PeerSet {
            peers: HashMap::new(),
        });

        // Clean expired peers first
        peer_set.cleanup(peer_ttl);

        // If at capacity and peer is new, remove oldest
        if peer_set.peers.len() >= max_peers
            && !peer_set.peers.contains_key(&peer)
            && let Some(oldest) = peer_set.oldest_peer()
        {
            peer_set.peers.remove(&oldest);
        }

        // Insert or update the peer
        peer_set.peers.insert(peer, Instant::now());
    }

    /// Get all non-expired peers for an infohash.
    ///
    /// Returns an empty vector if no peers are stored for this infohash.
    pub fn get_peers(&mut self, info_hash: &InfoHash) -> Vec<SocketAddrV4> {
        let peer_ttl = self.peer_ttl;

        let Some(peer_set) = self.store.get_mut(info_hash) else {
            return Vec::new();
        };

        // Clean expired peers
        peer_set.cleanup(peer_ttl);

        peer_set.peers.keys().copied().collect()
    }

    /// Check if we have any peers for an infohash.
    #[allow(dead_code)]
    pub fn has_peers(&mut self, info_hash: &InfoHash) -> bool {
        !self.get_peers(info_hash).is_empty()
    }

    /// Get the number of tracked torrents.
    #[allow(dead_code)]
    pub fn torrent_count(&self) -> usize {
        self.store.len()
    }
}

impl Default for PeerStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerSet {
    /// Remove peers that have expired (not announced within TTL).
    fn cleanup(&mut self, ttl: Duration) {
        self.peers.retain(|_, timestamp| timestamp.elapsed() < ttl);
    }

    /// Find the peer with the oldest timestamp.
    fn oldest_peer(&self) -> Option<SocketAddrV4> {
        self.peers
            .iter()
            .min_by_key(|(_, ts)| *ts)
            .map(|(addr, _)| *addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn test_info_hash(byte: u8) -> InfoHash {
        let mut bytes = [0u8; 20];
        bytes[0] = byte;
        InfoHash::new(bytes)
    }

    fn test_peer(last_octet: u8, port: u16) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, last_octet), port)
    }

    #[test]
    fn add_and_get_peers() {
        let mut store = PeerStore::new();
        let info_hash = test_info_hash(1);

        store.add_peer(info_hash, test_peer(1, 6881));
        store.add_peer(info_hash, test_peer(2, 6881));
        store.add_peer(info_hash, test_peer(3, 6881));

        let peers = store.get_peers(&info_hash);
        assert_eq!(peers.len(), 3);
    }

    #[test]
    fn update_existing_peer() {
        let mut store = PeerStore::new();
        let info_hash = test_info_hash(1);
        let peer = test_peer(1, 6881);

        store.add_peer(info_hash, peer);
        store.add_peer(info_hash, peer); // Update timestamp

        let peers = store.get_peers(&info_hash);
        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn empty_for_unknown_infohash() {
        let mut store = PeerStore::new();
        let info_hash = test_info_hash(99);

        let peers = store.get_peers(&info_hash);
        assert!(peers.is_empty());
    }

    #[test]
    fn respects_max_peers_limit() {
        let mut store = PeerStore::with_capacity(10, 3); // Max 3 peers per torrent
        let info_hash = test_info_hash(1);

        // Add 5 peers
        for i in 1..=5 {
            store.add_peer(info_hash, test_peer(i, 6881));
        }

        // Should only have 3 peers (the 3 most recently added)
        let peers = store.get_peers(&info_hash);
        assert_eq!(peers.len(), 3);
    }
}
