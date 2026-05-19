use crate::node_id::NodeId;
use std::{net::SocketAddr, time::Instant};

#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub status: ContactStatus,
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

pub const K: usize = 14;

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
    // TODO Use
    // buckets: BTreeMap<u8,KBucket>,
    buckets: [KBucket; 160],
    address_family: AddressFamily,
}

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

    fn find_bucket_index(&self, id: &NodeId) -> usize {
        self.local_id.distance_exp(id)
    }

    pub fn add_node(&mut self, node_id: NodeId, addr: SocketAddr) -> bool {
        let index = self.find_bucket_index(&node_id);
        if index == 0 {
            return false;
        }

        let bucket = &mut self.buckets[index];

        if let Some(entry) = bucket.nodes.iter_mut().find(|n| n.node_id == node_id) {
            entry.addr = addr;
            entry.reset_fail_count();
            entry.last_queried = Some(Instant::now());
            return true;
        }

        if bucket.nodes.len() < K {
            bucket.nodes.push(NodeEntry {
                node_id,
                addr,
                status: ContactStatus::Confirmed {
                    consecutive_failures: 0,
                },
                last_queried: None,
            });
            return true;
        }

        bucket.replacement_candidates.push(NodeEntry {
            node_id,
            addr,
            status: ContactStatus::Fresh,
            last_queried: None,
        });

        false
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
