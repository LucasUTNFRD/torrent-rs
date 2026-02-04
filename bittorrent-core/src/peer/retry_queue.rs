use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

use crate::peer::peer_connection::ConnectionError;

/// Maximum number of peers to track in the retry queue
const MAX_RETRY_QUEUE_SIZE: usize = 1000;

/// Maximum number of connection attempts before giving up on a peer
const MAX_RETRY_ATTEMPTS: u32 = 5;

/// Initial delay before first retry
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Maximum delay between retries (10 minutes)
const MAX_RETRY_DELAY: Duration = Duration::from_secs(600);

/// Entry for a peer that failed to connect
#[derive(Debug, Clone)]
pub struct FailedPeerEntry {
    pub addr: SocketAddr,
    pub failed_attempts: u32,
    pub last_attempt: Instant,
    pub next_retry: Instant,
    pub last_error: Option<String>,
}

/// Queue for managing peers that failed to connect, with exponential backoff
#[derive(Debug, Default)]
pub struct PeerRetryQueue {
    peers: HashMap<SocketAddr, FailedPeerEntry>,
}

impl PeerRetryQueue {
    /// Add a failed peer to the retry queue
    pub fn add_failed_peer(&mut self, addr: SocketAddr, error: Option<&ConnectionError>) {
        // Check if we've reached the max queue size
        if self.peers.len() >= MAX_RETRY_QUEUE_SIZE && !self.peers.contains_key(&addr) {
            // Remove oldest entry to make room
            if let Some(oldest) = self
                .peers
                .iter()
                .min_by_key(|(_, entry)| entry.last_attempt)
                .map(|(addr, _)| *addr)
            {
                self.peers.remove(&oldest);
            }
        }

        let now = Instant::now();

        if let Some(entry) = self.peers.get_mut(&addr) {
            // Update existing entry
            entry.failed_attempts += 1;
            entry.last_attempt = now;
            entry.next_retry = now + Self::calculate_backoff(entry.failed_attempts);
            if let Some(err) = error {
                entry.last_error = Some(err.to_string());
            }
        } else {
            // Create new entry
            let next_retry = now + INITIAL_RETRY_DELAY;
            let entry = FailedPeerEntry {
                addr,
                failed_attempts: 1,
                last_attempt: now,
                next_retry,
                last_error: error.map(|e| e.to_string()),
            };
            self.peers.insert(addr, entry);
        }
    }

    /// Get peers that are ready to be retried
    pub fn get_ready_peers(&mut self, now: Instant) -> Vec<SocketAddr> {
        let mut ready = Vec::new();
        let mut to_remove = Vec::new();

        for (addr, entry) in &self.peers {
            // Check if peer has exceeded max retry attempts
            if entry.failed_attempts >= MAX_RETRY_ATTEMPTS {
                to_remove.push(*addr);
                continue;
            }

            // Check if it's time to retry
            if now >= entry.next_retry {
                ready.push(*addr);
            }
        }

        // Remove peers that exceeded max attempts
        for addr in to_remove {
            self.peers.remove(&addr);
        }

        ready
    }

    /// Check if a peer should be retried (exists in queue and hasn't exceeded max attempts)
    pub fn should_retry(&self, addr: &SocketAddr) -> bool {
        if let Some(entry) = self.peers.get(addr) {
            entry.failed_attempts < MAX_RETRY_ATTEMPTS
        } else {
            false
        }
    }

    /// Remove a peer from the retry queue (e.g., when successfully connected)
    pub fn remove_peer(&mut self, addr: &SocketAddr) {
        self.peers.remove(addr);
    }

    /// Check if a peer is in the retry queue
    pub fn contains(&self, addr: &SocketAddr) -> bool {
        self.peers.contains_key(addr)
    }

    /// Get the number of peers in the retry queue
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Check if the retry queue is empty
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Calculate backoff delay using exponential backoff
    fn calculate_backoff(attempts: u32) -> Duration {
        let exponent = attempts.saturating_sub(1);
        let delay = INITIAL_RETRY_DELAY * 2_u32.pow(exponent);
        delay.min(MAX_RETRY_DELAY)
    }

    /// Mark a peer as successfully connected (removes from retry queue)
    pub fn mark_success(&mut self, addr: &SocketAddr) {
        self.remove_peer(addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_backoff() {
        assert_eq!(PeerRetryQueue::calculate_backoff(1), Duration::from_secs(5));
        assert_eq!(
            PeerRetryQueue::calculate_backoff(2),
            Duration::from_secs(10)
        );
        assert_eq!(
            PeerRetryQueue::calculate_backoff(3),
            Duration::from_secs(20)
        );
        assert_eq!(
            PeerRetryQueue::calculate_backoff(4),
            Duration::from_secs(40)
        );
        assert_eq!(
            PeerRetryQueue::calculate_backoff(5),
            Duration::from_secs(80)
        );
    }

    #[test]
    fn test_add_and_get_ready_peers() {
        let mut queue = PeerRetryQueue::default();
        let addr = SocketAddr::from(([127, 0, 0, 1], 6881));

        queue.add_failed_peer(addr, None);
        assert!(queue.contains(&addr));

        // Immediately after adding, peer should not be ready (needs to wait INITIAL_RETRY_DELAY)
        let now = Instant::now();
        let ready = queue.get_ready_peers(now);
        assert!(ready.is_empty());

        // After enough time passes, peer should be ready
        let future = now + Duration::from_secs(10);
        let ready = queue.get_ready_peers(future);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0], addr);
    }

    #[test]
    fn test_max_retry_attempts() {
        let mut queue = PeerRetryQueue::default();
        let addr = SocketAddr::from(([127, 0, 0, 1], 6881));

        // Add peer MAX_RETRY_ATTEMPTS times
        for _ in 0..MAX_RETRY_ATTEMPTS {
            queue.add_failed_peer(addr, None);
        }

        // Peer should still be in queue
        assert!(queue.contains(&addr));

        // Add one more time to exceed max attempts
        queue.add_failed_peer(addr, None);

        // Now when we get ready peers, it should be removed
        let future = Instant::now() + Duration::from_secs(1000);
        let ready = queue.get_ready_peers(future);
        assert!(ready.is_empty());
        assert!(!queue.contains(&addr));
    }
}
