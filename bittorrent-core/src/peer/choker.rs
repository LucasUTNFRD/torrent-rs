use rand::{rng, seq::IndexedRandom};
use std::collections::{HashMap, HashSet};
use tokio::time::Instant;

use crate::peer::manager::Id;

pub struct Choker {
    /// Currently unchoked peers
    unchoked: HashSet<Id>,
    /// Peers that have expressed interest
    pub interested: HashSet<Id>,
    /// Round counter for optimistic unchoke timing
    round_counter: u32,
    /// The peer selected for optimistic unchoke (lasts for 3 rounds)
    planned_optimistic_peer: Option<Id>,
}

impl Choker {
    pub fn new() -> Self {
        Self {
            unchoked: HashSet::new(),
            interested: HashSet::new(),
            round_counter: 0,
            planned_optimistic_peer: None,
        }
    }
}
//
//     /// Main choke algorithm - runs every 10 seconds or when peer set changes
//     pub fn run_choke_algorithm(&mut self, peers: &HashMap<Id, PeerStats>) -> (Vec<Id>, Vec<Id>) {
//         self.round_counter += 1;
//
//         // Step 1: Optimistic Unchoke (every 3rd round = 30 seconds)
//         let is_optimistic_round = self.round_counter % 3 == 1;
//         if is_optimistic_round {
//             self.select_optimistic_unchoke_peer(peers);
//         }
//
//         // Step 2: Regular Unchoke - find 3 fastest peers
//         let regular_unchoked = self.select_regular_unchoked_peers(peers);
//
//         // Step 3 & 4: Handle optimistic unchoke logic
//         let final_unchoked = self.finalize_unchoked_peers(peers, regular_unchoked);
//
//         // Update the unchoked set and send choke/unchoke messages
//         self.update_unchoked_peers(final_unchoked)
//     }
//
//     /// Step 1: Select a random choked and interested peer for optimistic unchoke
//     fn select_optimistic_unchoke_peer(&mut self, peers: &HashMap<Id, PeerStats>) {
//         let choked_interested: Vec<Id> = self
//             .interested
//             .iter()
//             .filter(|&&peer_id| !self.unchoked.contains(&peer_id))
//             .filter(|&&peer_id| peers.contains_key(&peer_id))
//             .copied()
//             .collect();
//
//         if !choked_interested.is_empty() {
//             let mut rng = rand::rng();
//             if let Some(&selected) = choked_interested.choose(&mut rng) {
//                 self.planned_optimistic_peer = Some(selected);
//                 // tracing::debug!("Selected peer {} for optimistic unchoke", selected.0);
//             }
//         }
//     }
//
//     /// Step 2: Select the 3 fastest peers that sent blocks in last 30 seconds
//     fn select_regular_unchoked_peers(&self, peers: &HashMap<Id, PeerStats>) -> Vec<Id> {
//         let now = Instant::now();
//         let thirty_seconds_ago = now - std::time::Duration::from_secs(30);
//
//         // Filter interested peers that sent blocks in last 30 seconds (not snubbed)
//         let mut active_peers: Vec<(Id, usize)> = self
//             .interested
//             .iter()
//             .filter(|&&peer_id| {
//                 if let Some(stats) = peers.get(&peer_id) {
//                     stats.last_block > thirty_seconds_ago && stats.downloaded > 0
//                 } else {
//                     false
//                 }
//             })
//             .map(|&peer_id| {
//                 let download_rate = peers
//                     .get(&peer_id)
//                     .map(|stats| stats.downloaded)
//                     .unwrap_or(0);
//                 (peer_id, download_rate)
//             })
//             .collect();
//
//         // Sort by download rate (fastest first)
//         active_peers.sort_by(|a, b| b.1.cmp(&a.1));
//
//         // Take top 3
//         active_peers
//             .into_iter()
//             .take(3)
//             .map(|(peer_id, _)| peer_id)
//             .collect()
//     }
//
//     /// Steps 3 & 4: Finalize the unchoked peer set
//     fn finalize_unchoked_peers(
//         &mut self,
//         peers: &HashMap<Id, PeerStats>,
//         regular_unchoked: Vec<Id>,
//     ) -> HashSet<Id> {
//         let mut final_unchoked: HashSet<Id> = regular_unchoked.into_iter().collect();
//
//         if let Some(optimistic_peer) = self.planned_optimistic_peer {
//             // Step 3: Check if optimistic peer is in the regular unchoked set
//             if final_unchoked.contains(&optimistic_peer) {
//                 // Step 4: Optimistic peer is already unchoked, find another one
//                 self.find_additional_optimistic_peer(peers, &mut final_unchoked);
//             } else {
//                 // Optimistic peer is not in regular set, add it if interested
//                 if self.interested.contains(&optimistic_peer)
//                     && peers.contains_key(&optimistic_peer)
//                 {
//                     final_unchoked.insert(optimistic_peer);
//                     // tracing::debug!(
//                     //     "Added planned optimistic peer {} to unchoked set",
//                     //     optimistic_peer.0
//                     // );
//                 }
//             }
//         }
//
//         final_unchoked
//     }
//
//     /// Step 4: Find additional optimistic unchoke peer when planned one is already unchoked
//     fn find_additional_optimistic_peer(
//         &self,
//         peers: &HashMap<Id, PeerStats>,
//         final_unchoked: &mut HashSet<Id>,
//     ) {
//         let mut rng = rng();
//         let mut attempts = 0;
//         let max_attempts = 10; // Prevent infinite loops
//
//         while attempts < max_attempts {
//             // Get all choked peers (not in final_unchoked set)
//             let choked_peers: Vec<Id> = peers
//                 .keys()
//                 .filter(|&&peer_id| !final_unchoked.contains(&peer_id))
//                 .copied()
//                 .collect();
//
//             if choked_peers.is_empty() {
//                 break;
//             }
//
//             if let Some(&random_peer) = choked_peers.choose(&mut rng) {
//                 if self.interested.contains(&random_peer) {
//                     final_unchoked.insert(random_peer);
//                     // tracing::debug!(
//                     //     "Added additional optimistic peer {} to unchoked set",
//                     //     random_peer.0
//                     // );
//                     break;
//                 } else {
//                     // Peer not interested, try again
//                     attempts += 1;
//                 }
//             } else {
//                 break;
//             }
//         }
//     }
//
//     /// Update the internal unchoked set and return peers that need choke/unchoke messages
//     pub fn update_unchoked_peers(&mut self, new_unchoked: HashSet<Id>) -> (Vec<Id>, Vec<Id>) {
//         // Peers to choke: currently unchoked but not in new set
//         let to_choke: Vec<Id> = self.unchoked.difference(&new_unchoked).copied().collect();
//
//         // Peers to unchoke: in new set but not currently unchoked
//         let to_unchoke: Vec<Id> = new_unchoked.difference(&self.unchoked).copied().collect();
//
//         // Update the unchoked set
//         self.unchoked = new_unchoked;
//
//         tracing::debug!(
//             "Choke algorithm completed. Unchoked: {:?}, To choke: {:?}, To unchoke: {:?}",
//             self.unchoked.iter().map(|id| id.0).collect::<Vec<_>>(),
//             to_choke.iter().map(|id| id.0).collect::<Vec<_>>(),
//             to_unchoke.iter().map(|id| id.0).collect::<Vec<_>>()
//         );
//
//         (to_choke, to_unchoke)
//     }
//
//     /// Get currently unchoked peers
//     pub fn get_unchoked(&self) -> &HashSet<Id> {
//         &self.unchoked
//     }
//
//     /// Check if a peer is unchoked
//     pub fn is_unchoked(&self, peer_id: &Id) -> bool {
//         self.unchoked.contains(peer_id)
//     }
//
//     pub fn contains_interested_peers(&self) -> bool {
//         self.unchoked.is_empty()
//     }
//
//     /// Remove a peer from all internal state (when peer disconnects)
//     pub fn remove_peer(&mut self, peer_id: &Id) {
//         self.unchoked.remove(peer_id);
//         self.interested.remove(peer_id);
//
//         // Clear planned optimistic peer if it's the one being removed
//         if self.planned_optimistic_peer == Some(*peer_id) {
//             self.planned_optimistic_peer = None;
//         }
//     }
//
//     /// Add or remove peer interest
//     pub fn set_peer_interest(&mut self, peer_id: Id, interested: bool) {
//         if interested {
//             self.interested.insert(peer_id);
//         } else {
//             self.interested.remove(&peer_id);
//         }
//     }
// }
//
// #[cfg(test)]
// mod tests {
//     use super::*;
//     use std::time::Duration;
//
//     fn create_test_peer_stats(downloaded: usize, seconds_ago: u64) -> PeerStats {
//         PeerStats {
//             uploaded: 0,
//             downloaded,
//             last_block: Instant::now() - Duration::from_secs(seconds_ago),
//         }
//     }
//
//     #[test]
//     fn test_regular_unchoke_selection() {
//         let mut choker = Choker::new();
//         let mut peers = HashMap::new();
//
//         // Create test peers
//         let peer1 = Id(1);
//         let peer2 = Id(2);
//         let peer3 = Id(3);
//         let peer4 = Id(4);
//
//         // Add peer stats (downloaded, seconds_since_last_block)
//         peers.insert(peer1, create_test_peer_stats(1000, 10)); // Fast, recent
//         peers.insert(peer2, create_test_peer_stats(500, 15)); // Medium, recent
//         peers.insert(peer3, create_test_peer_stats(2000, 5)); // Fastest, recent
//         peers.insert(peer4, create_test_peer_stats(3000, 40)); // Fast but snubbed
//
//         // Mark all as interested
//         choker.interested.insert(peer1);
//         choker.interested.insert(peer2);
//         choker.interested.insert(peer3);
//         choker.interested.insert(peer4);
//
//         let regular_unchoked = choker.select_regular_unchoked_peers(&peers);
//
//         // Should select peer3, peer1, peer2 (fastest first, excluding snubbed peer4)
//         assert_eq!(regular_unchoked.len(), 3);
//         assert!(regular_unchoked.contains(&peer3)); // Fastest
//         assert!(regular_unchoked.contains(&peer1)); // Second fastest
//         assert!(regular_unchoked.contains(&peer2)); // Third fastest
//         assert!(!regular_unchoked.contains(&peer4)); // Snubbed
//     }
//
//     #[test]
//     fn test_optimistic_unchoke_timing() {
//         let mut choker = Choker::new();
//         let peers = HashMap::new();
//
//         // First round should select optimistic peer
//         assert_eq!(choker.round_counter, 0);
//         choker.run_choke_algorithm(&peers);
//         assert_eq!(choker.round_counter, 1);
//
//         // Second round should not select new optimistic peer
//         choker.run_choke_algorithm(&peers);
//         assert_eq!(choker.round_counter, 2);
//
//         // Third round should not select new optimistic peer
//         choker.run_choke_algorithm(&peers);
//         assert_eq!(choker.round_counter, 3);
//
//         // Fourth round should select new optimistic peer (round_counter % 3 == 1)
//         choker.run_choke_algorithm(&peers);
//         assert_eq!(choker.round_counter, 4);
//     }
// }
