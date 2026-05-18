use crate::node_id::NodeId;
use std::{net::SocketAddr, time::Instant};

#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub status: ContactStatus,
    pub verified: bool,
    pub last_queried: Option<std::time::Instant>,
}

#[derive(Debug, PartialEq, Clone)]
pub enum ContactStatus {
    /// Never sent a request to this node
    Fresh,
    /// Sent at least one request, last one succeeded
    Confirmed { consecutive_failures: u8 },
}

impl NodeEntry {
    pub const fn pinged(&self) -> bool {
        matches!(self.status, ContactStatus::Confirmed { .. })
    }

    pub fn confirmed(&self) -> bool {
        self.status
            == ContactStatus::Confirmed {
                consecutive_failures: 0,
            }
    }

    pub const fn timed_out(&mut self) {
        if let ContactStatus::Confirmed {
            ref mut consecutive_failures,
        } = self.status
        {
            *consecutive_failures = consecutive_failures.saturating_add(1);
        }
    }

    pub fn set_pinged(&mut self) {
        if self.status == ContactStatus::Fresh {
            self.status = ContactStatus::Confirmed {
                consecutive_failures: 0,
            };
        }
    }

    pub const fn reset_fail_count(&mut self) {
        if let ContactStatus::Confirmed {
            ref mut consecutive_failures,
        } = self.status
        {
            *consecutive_failures = 0;
        }
    }

    pub const fn fail_count(&self) -> u8 {
        match self.status {
            ContactStatus::Confirmed {
                consecutive_failures,
            } => consecutive_failures,
            ContactStatus::Fresh => 0,
        }
    }

    // TODO : IsGood fn
    // 	return time.Since(n.lastGotResponse) < 15*time.Minute ||
    // !n.lastGotResponse.IsZero() && time.Since(n.lastGotQuery) < 15*time.Minute
}

#[derive(Debug)]
struct KBucket {
    pub nodes: Vec<NodeEntry>,
    pub replacement_candidates: Vec<NodeEntry>,
}

const K: usize = 8;

impl KBucket {
    fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(K),
            replacement_candidates: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    V6,
    V4,
}

#[derive(Debug)]
pub struct RoutingTable {
    local_id: NodeId,
    buckets: [KBucket; 160],
    address_family: AddressFamily,
}

// Routing Table Maintenance (The "Heartbeat")
//   This is the most critical task. It ensures your "address book" doesn't become filled with dead nodes.
//
//    * Bucket Refreshing (Active):
//        * Logic: Every bucket that hasn't seen a lookup in the last 15 minutes must be refreshed.
//        * Action: Find the node that hasn't been queried for the longest time (last_queried). Generate a random ID within its bucket's range and perform a find_node or get_peers search for that ID.
//    * Neighborhood Deepening (Self-Bootstrap):
//        * Condition: If your routing table depth is low (e.g., depth < 4), your knowledge of your immediate neighbors is too thin.
//        * Action: Trigger a bootstrap search for your own Node ID.
//    * Stale Node Pruning:
//        * Logic: If a node's timeout_count reaches your threshold (libtorrent uses 20, but some use 3 or 5 for faster cleanup), it is dead.
//        * Action: Remove it. If you have a healthy node in the replacements cache for that bucket, promote it to the active list immediately.
//
//   2. Security Maintenance
//   These tasks protect your node from being used in amplification attacks or token-hijacking.
//
//    * Token Secret Rotation:
//        * Logic: Libtorrent rotates the "write token" secret every 5 minutes.
//        * Action: Generate a new random secret. Keep the previous secret for another 5 minutes (so that in-flight requests don't fail) but use the new one for all new requests.
//    * DoS Blocker Cleanup:
//        * Logic: Your DoS blocker keeps a list of "banned" IPs.
//        * Action: Periodically clear entries for IPs that have stopped flooding you so they can eventually rejoin the network.
//
//   3. Traffic & Resource Management
//    * Rate Limit Replenishment:
//        * Logic: You likely have an upload quota (e.g., 8KB/s).
//        * Action: Every tick, add "tokens" to your token-bucket based on how much time has passed.
//    * Round-Trip Time (RTT) Decay:
//        * Logic: Latency changes over time.
//        * Action: Libtorrent uses an exponential moving average. When a node responds, update its RTT: new_rtt = (old_rtt * 2 + measured_rtt) / 3.
//
//   4. Data Storage Maintenance (The storage tick)
//   If your Rust port will also store peers (becoming a "tracker"), you must clean up data periodically (usually every 2 minutes):
//
//    * Peer Expiration: Standard BitTorrent peers expire from the DHT after 30 minutes. If they haven't "re-announced," delete them.
//    * Item Expiration: For BEP 44 (mutable/immutable items), items typically expire after 2 hours unless they are re-put.

impl RoutingTable {
    fn new(local_id: NodeId, family: AddressFamily) -> Self {
        Self {
            local_id,
            buckets: std::array::from_fn(|_| KBucket::new()),
            address_family: family,
        }
    }

    pub fn new_v4(local_id: NodeId) -> Self {
        Self::new(local_id, AddressFamily::V4)
    }

    pub fn new_v6(local_id: NodeId) -> Self {
        Self::new(local_id, AddressFamily::V6)
    }

    /// 1. Find the bucket index for a target ID
    fn find_bucket_index(&self, id: &NodeId) -> usize {
        self.local_id.distance_exp(id)
    }

    /// 2. Add or update a node in the table
    /// Logic:
    /// - If node exists: update last seen,
    /// - If bucket not full: insert.
    /// - TODO: Discuss what policy implement, it comes to my mind adding the node id if full as a
    /// candidate
    pub fn add_node(&mut self, node_id: NodeId, addr: SocketAddr) -> AddResult {
        let index = self.find_bucket_index(&node_id);
        let bucket = &mut self.buckets[index];

        if let Some(entry) = bucket.nodes.iter_mut().find(|n| n.node_id == node_id) {
            entry.addr = addr;
            entry.reset_fail_count();
            entry.last_queried = Some(Instant::now());
            return AddResult::Updated;
        }

        if bucket.nodes.len() < K {
            bucket.nodes.push(NodeEntry {
                node_id,
                addr,
                status: ContactStatus::Confirmed {
                    consecutive_failures: 0,
                },
                verified: true,
                last_queried: None,
            });
            return AddResult::Added;
        }

        bucket.replacement_candidates.push(NodeEntry {
            node_id,
            addr,
            status: ContactStatus::Fresh,
            verified: false,
            last_queried: None,
        });

        AddResult::MarkedAsAck
    }

    pub fn find_closest(&self, target: &NodeId, count: usize) -> Vec<NodeEntry> {
        let mut all_nodes: Vec<NodeEntry> = self
            .buckets
            .iter()
            .flat_map(|b| b.nodes.iter().cloned())
            .collect();

        all_nodes.sort_by(|a, b| {
            let da = a.node_id.distance(target);
            let db = b.node_id.distance(target);
            da.cmp(&db)
        });

        all_nodes.truncate(count);
        all_nodes
    }

    pub fn size(&self) -> usize {
        self.buckets.iter().map(|b| b.nodes.len()).sum()
    }
}

pub enum AddResult {
    Added,
    Updated,
    MarkedAsAck,
}
