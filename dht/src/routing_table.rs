use crate::node_id::NodeId;
use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub(crate) struct NodeEntry {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub status: ContactStatus,
    pub verified: bool,
    pub last_queried: Option<std::time::Instant>,
}

#[derive(Debug, PartialEq, Clone)]
pub(crate) enum ContactStatus {
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

#[derive(Debug, Clone, Copy)]
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
    pub fn add_node(&mut self, node_id: NodeId, addr: SocketAddr) -> AddResult {
        let index = self.find_bucket_index(&node_id);
        let bucket = &mut self.buckets[index];

        if let Some(entry) = bucket.nodes.iter_mut().find(|n| n.node_id == node_id) {
            entry.addr = addr;
            entry.reset_fail_count();
            return AddResult::Added;
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

        AddResult::Failed
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
    Failed,
    Added,
    NeedSplit,
}
