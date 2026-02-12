//! The choker is responsible for:
//! - Deciding which peers receive upload bandwidth (unchoked)
//! - Which peers are denied upload bandwidth (choked)
//! - Managing upload slots dynamically
//! - Tracking peer performance metrics

use std::collections::{HashSet, VecDeque};

use crate::torrent::Pid;

/// Simple choker that maintains a fixed number of upload slots.
/// Uses round-robin for slot assignment when there are more interested peers than slots.
#[derive(Debug)]
pub struct Choker {
    /// Maximum number of peers to unchoke simultaneously
    upload_slots: usize,
    /// Set of peers that are currently unchoked
    unchoked_peers: HashSet<Pid>,
    /// Queue of interested peers waiting for a slot (FIFO for round-robin)
    interested_queue: VecDeque<Pid>,
    /// Set of peers that have expressed interest (currently interested)
    interested_peers: HashSet<Pid>,
}

impl Choker {
    #[must_use]
    pub fn new(upload_slots: usize) -> Self {
        Self {
            upload_slots,
            unchoked_peers: HashSet::new(),
            interested_queue: VecDeque::new(),
            interested_peers: HashSet::new(),
        }
    }

    /// Called when a peer expresses interest in our pieces
    /// Returns true if the peer should be unchoked immediately
    pub fn on_peer_interested(&mut self, pid: Pid) -> bool {
        // Don't track if already interested
        if self.interested_peers.contains(&pid) {
            return self.unchoked_peers.contains(&pid);
        }

        self.interested_peers.insert(pid);

        // If we have available slots, unchoke immediately
        if self.unchoked_peers.len() < self.upload_slots {
            self.unchoked_peers.insert(pid);
            return true;
        }

        // Otherwise, add to queue for round-robin
        self.interested_queue.push_back(pid);
        false
    }

    /// Called when a peer is no longer interested in our pieces
    /// Returns true if the peer was unchoked and should now be choked
    pub fn on_peer_not_interested(&mut self, pid: Pid) -> bool {
        // Remove from interested set
        self.interested_peers.remove(&pid);

        // Remove from queue if waiting
        if let Some(pos) = self.interested_queue.iter().position(|&p| p == pid) {
            self.interested_queue.remove(pos);
        }

        // If peer was unchoked, remove it and potentially unchoke next in queue
        let was_unchoked = self.unchoked_peers.remove(&pid);

        if was_unchoked {
            // Try to unchoke the next peer in queue
            while let Some(next_pid) = self.interested_queue.pop_front() {
                // Only unchoke if still interested
                if self.interested_peers.contains(&next_pid) {
                    self.unchoked_peers.insert(next_pid);
                    break;
                }
            }
        }

        was_unchoked
    }

    /// Called when a peer disconnects
    pub fn on_peer_disconnected(&mut self, pid: Pid) {
        self.interested_peers.remove(&pid);
        self.interested_queue.retain(|&p| p != pid);

        let was_unchoked = self.unchoked_peers.remove(&pid);

        // If we freed up a slot, unchoke next peer in queue
        if was_unchoked {
            while let Some(next_pid) = self.interested_queue.pop_front() {
                if self.interested_peers.contains(&next_pid) {
                    self.unchoked_peers.insert(next_pid);
                    break;
                }
            }
        }
    }

    /// Returns true if the peer is currently unchoked
    #[must_use]
    pub fn is_unchoked(&self, pid: Pid) -> bool {
        self.unchoked_peers.contains(&pid)
    }

    /// Returns the number of available upload slots
    #[must_use]
    pub fn available_slots(&self) -> usize {
        self.upload_slots.saturating_sub(self.unchoked_peers.len())
    }

    /// Returns the set of currently unchoked peers
    #[must_use]
    pub fn unchoked_peers(&self) -> &HashSet<Pid> {
        &self.unchoked_peers
    }

    /// Periodic re-evaluation of unchoke decisions.
    /// Implements round-robin rotation of upload slots.
    /// Returns (peers_to_choke, peers_to_unchoke) to apply changes.
    pub fn re_evaluate_unchokes(&mut self) -> (Vec<Pid>, Vec<Pid>) {
        let mut to_choke = Vec::new();
        let mut to_unchoke = Vec::new();

        // Only rotate if we have more interested peers than slots
        if self.interested_peers.len() <= self.upload_slots {
            return (to_choke, to_unchoke);
        }

        // Find the first unchoked peer to rotate out (oldest)
        if let Some(rotate_out) = self.unchoked_peers.iter().copied().next() {
            // Find the next interested peer in queue to rotate in
            if let Some(rotate_in) = self.interested_queue.pop_front() {
                // Verify rotate_in is still interested
                if self.interested_peers.contains(&rotate_in) {
                    to_choke.push(rotate_out);
                    to_unchoke.push(rotate_in);

                    self.unchoked_peers.remove(&rotate_out);
                    self.unchoked_peers.insert(rotate_in);

                    // Put the rotated out peer back in the queue
                    if self.interested_peers.contains(&rotate_out) {
                        self.interested_queue.push_back(rotate_out);
                    }
                } else {
                    // rotate_in lost interest, try next
                    self.interested_queue.push_back(rotate_out);
                }
            }
        }

        (to_choke, to_unchoke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_interest() {
        let mut choker = Choker::new(4);

        let pid1 = Pid(1);
        let pid2 = Pid(2);

        // Both interested, within slot limit
        assert!(choker.on_peer_interested(pid1));
        assert!(choker.on_peer_interested(pid2));

        assert!(choker.is_unchoked(pid1));
        assert!(choker.is_unchoked(pid2));
    }

    #[test]
    fn test_slot_limit() {
        let mut choker = Choker::new(2);

        let pid1 = Pid(1);
        let pid2 = Pid(2);
        let pid3 = Pid(3);

        // First two get slots
        assert!(choker.on_peer_interested(pid1));
        assert!(choker.on_peer_interested(pid2));

        // Third goes to queue
        assert!(!choker.on_peer_interested(pid3));
        assert!(!choker.is_unchoked(pid3));
    }

    #[test]
    fn test_not_interested_frees_slot() {
        let mut choker = Choker::new(2);

        let pid1 = Pid(1);
        let pid2 = Pid(2);
        let pid3 = Pid(3);

        choker.on_peer_interested(pid1);
        choker.on_peer_interested(pid2);
        choker.on_peer_interested(pid3); // Queued

        assert!(choker.is_unchoked(pid1));
        assert!(choker.is_unchoked(pid2));
        assert!(!choker.is_unchoked(pid3));

        // pid1 becomes not interested
        assert!(choker.on_peer_not_interested(pid1));
        assert!(!choker.is_unchoked(pid1));

        // pid3 should now be unchoked
        assert!(choker.is_unchoked(pid3));
    }

    #[test]
    fn test_disconnect() {
        let mut choker = Choker::new(2);

        let pid1 = Pid(1);
        let pid2 = Pid(2);
        let pid3 = Pid(3);

        choker.on_peer_interested(pid1);
        choker.on_peer_interested(pid2);
        choker.on_peer_interested(pid3); // Queued

        // pid2 disconnects
        choker.on_peer_disconnected(pid2);

        assert!(choker.is_unchoked(pid1));
        assert!(!choker.is_unchoked(pid2));
        assert!(choker.is_unchoked(pid3)); // Should get the slot
    }

    #[test]
    fn test_periodic_re_evaluate() {
        let mut choker = Choker::new(2);

        let pid1 = Pid(1);
        let pid2 = Pid(2);
        let pid3 = Pid(3);

        // Three peers interested, only 2 slots
        choker.on_peer_interested(pid1);
        choker.on_peer_interested(pid2);
        choker.on_peer_interested(pid3); // Queued

        assert!(choker.is_unchoked(pid1));
        assert!(choker.is_unchoked(pid2));
        assert!(!choker.is_unchoked(pid3));

        // Periodic re-evaluation should rotate slots
        let (to_choke, to_unchoke) = choker.re_evaluate_unchokes();

        // One peer should be choked and one unchoked
        assert_eq!(to_choke.len(), 1);
        assert_eq!(to_unchoke.len(), 1);

        // pid3 should now be unchoked
        assert!(choker.is_unchoked(pid3));

        // One of pid1 or pid2 should be choked
        let choked_count = [!choker.is_unchoked(pid1), !choker.is_unchoked(pid2)]
            .iter()
            .filter(|&&x| x)
            .count();
        assert_eq!(choked_count, 1);
    }

    #[test]
    fn test_re_evaluate_no_rotation_when_under_limit() {
        let mut choker = Choker::new(4);

        let pid1 = Pid(1);
        let pid2 = Pid(2);

        // Two peers interested, 4 slots available
        choker.on_peer_interested(pid1);
        choker.on_peer_interested(pid2);

        assert!(choker.is_unchoked(pid1));
        assert!(choker.is_unchoked(pid2));

        // No rotation needed since we have more slots than peers
        let (to_choke, to_unchoke) = choker.re_evaluate_unchokes();

        assert!(to_choke.is_empty());
        assert!(to_unchoke.is_empty());

        assert!(choker.is_unchoked(pid1));
        assert!(choker.is_unchoked(pid2));
    }
}
