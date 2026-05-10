use crate::node_id::NodeId;
use std::net::SocketAddr;

#[derive(Debug)]
struct NodeEntry {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub status: ContactStatus,
    pub verified: bool,
    pub last_queried: Option<std::time::Instant>,
}

#[derive(Debug, PartialEq)]
enum ContactStatus {
    /// Never sent a request to this node
    Fresh,
    /// Sent at least one request, last one succeeded
    Confirmed { consecutive_failures: u8 },
}

impl NodeEntry {
    pub fn pinged(&self) -> bool {
        matches!(self.status, ContactStatus::Confirmed { .. })
    }

    pub fn confirmed(&self) -> bool {
        self.status
            == ContactStatus::Confirmed {
                consecutive_failures: 0,
            }
    }

    pub fn timed_out(&mut self) {
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

    pub fn reset_fail_count(&mut self) {
        if let ContactStatus::Confirmed {
            ref mut consecutive_failures,
        } = self.status
        {
            *consecutive_failures = 0;
        }
    }

    pub fn fail_count(&self) -> u8 {
        match self.status {
            ContactStatus::Confirmed {
                consecutive_failures,
            } => consecutive_failures,
            ContactStatus::Fresh => 0,
        }
    }
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

#[derive(Debug, Clone)]
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
    pub fn add_node(&mut self, node: NodeId) -> AddResult {
        todo!()
    }

    pub fn find_closest(&self, target: &NodeId, count: usize) -> Vec<NodeEntry> {
        todo!()
    }
}

enum AddResult {
    Failed,
    Added,
    NeedSplit,
}
