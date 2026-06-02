use crate::node_id::NodeId;
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub status: ContactStatus,
    /// The timestamp of the last successful interaction (when they responded to us,
    /// or when they successfully queried us).
    pub last_seen: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ContactStatus {
    /// Never successfully sent/received a request to/from this node
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

    pub fn timed_out(&mut self) {
        if let ContactStatus::Confirmed {
            ref mut consecutive_failures,
        } = self.status
        {
            *consecutive_failures = consecutive_failures.saturating_add(1);
        } else if self.status == ContactStatus::Fresh {
            self.status = ContactStatus::Confirmed {
                consecutive_failures: 1,
            };
        }
    }

    pub fn set_pinged(&mut self) {
        self.status = ContactStatus::Confirmed {
            consecutive_failures: 0,
        };
        self.last_seen = Some(Instant::now());
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

    /// A node is considered "Good" if it has responded to a query or sent a query
    /// to us in the last 15 minutes.
    pub fn is_good(&self) -> bool {
        self.confirmed() && self.last_seen.is_some_and(|seen| {
            seen.elapsed() < Duration::from_secs(15 * 60)
        })
    }

    /// A node is considered "Bad" if it has failed to respond to multiple consecutive queries (2 or more).
    pub const fn is_bad(&self) -> bool {
        self.fail_count() >= 2
    }

    /// A node is considered "Questionable" if it hasn't responded or queried in the last 15 minutes,
    /// but hasn't failed to respond to multiple consecutive queries either.
    pub fn is_questionable(&self) -> bool {
        !self.is_good() && !self.is_bad()
    }
}

#[derive(Debug)]
pub struct KBucket {
    pub nodes: Vec<NodeEntry>,
    pub replacement_candidates: Vec<NodeEntry>,
    pub last_changed: Instant,
}

pub const K: usize = 8;

impl KBucket {
    fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(K),
            replacement_candidates: Vec::new(),
            last_changed: Instant::now(),
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
    pub(crate) buckets: [KBucket; 160],
    address_family: AddressFamily,
}

#[derive(Debug, Clone)]
pub enum AddNodeResult {
    /// The node was successfully added or updated in the main bucket.
    Added,
    /// The bucket is full, but we identified a questionable node that should be pinged.
    /// Contains the questionable node to ping.
    PingQuestionable {
        questionable_node: NodeEntry,
    },
    /// The node was added to the replacement candidates because the bucket is full and all nodes are good.
    Cached,
    /// Ignored (e.g. self ID).
    Ignored,
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

    pub fn add_node(&mut self, node_id: NodeId, addr: SocketAddr, is_response: bool) -> AddNodeResult {
        let index = self.find_bucket_index(&node_id);
        if index == 0 {
            return AddNodeResult::Ignored;
        }

        let bucket = &mut self.buckets[index];

        // 1. If node already exists, update address and reset fail count
        if let Some(entry) = bucket.nodes.iter_mut().find(|n| n.node_id == node_id) {
            entry.addr = addr;
            if is_response {
                entry.set_pinged();
            } else {
                entry.last_seen = Some(Instant::now());
            }
            bucket.last_changed = Instant::now();

            // If the bucket is full and we have pending candidates, see if there's another questionable node to ping
            if bucket.nodes.len() == K && !bucket.replacement_candidates.is_empty() {
                let oldest_questionable = bucket
                    .nodes
                    .iter()
                    .filter(|n| n.is_questionable())
                    .min_by_key(|n| n.last_seen.unwrap_or_else(Instant::now));

                if let Some(questionable) = oldest_questionable {
                    return AddNodeResult::PingQuestionable {
                        questionable_node: questionable.clone(),
                    };
                }
            }

            return AddNodeResult::Added;
        }

        let initial_status = if is_response {
            ContactStatus::Confirmed { consecutive_failures: 0 }
        } else {
            ContactStatus::Fresh
        };

        if bucket.nodes.len() < K {
            bucket.nodes.push(NodeEntry {
                node_id,
                addr,
                status: initial_status.clone(),
                last_seen: Some(Instant::now()),
            });
            bucket.last_changed = Instant::now();
            return AddNodeResult::Added;
        }

        if let Some(bad_pos) = bucket.nodes.iter().position(NodeEntry::is_bad) {
            bucket.nodes.remove(bad_pos);
            bucket.nodes.push(NodeEntry {
                node_id,
                addr,
                status: initial_status.clone(),
                last_seen: Some(Instant::now()),
            });
            bucket.last_changed = Instant::now();
            return AddNodeResult::Added;
        }

        // 4. If all existing nodes are Good, add to replacement cache
        if bucket.nodes.iter().all(NodeEntry::is_good) {
            if !bucket.replacement_candidates.iter().any(|n| n.node_id == node_id) {
                bucket.replacement_candidates.push(NodeEntry {
                    node_id,
                    addr,
                    status: initial_status,
                    last_seen: Some(Instant::now()),
                });
            }
            return AddNodeResult::Cached;
        }

        // 4. Check if any existing node is Questionable. Find the oldest questionable node to ping.
        // We find the one with the earliest last_seen timestamp.
        let oldest_questionable = bucket
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_questionable())
            .min_by_key(|(_, n)| n.last_seen.unwrap_or_else(Instant::now));

        let candidate = NodeEntry {
            node_id,
            addr,
            status: ContactStatus::Fresh,
            last_seen: None,
        };

        // Cache the candidate in replacement_candidates (limit to K entries to avoid unbounded growth)
        if !bucket
            .replacement_candidates
            .iter()
            .any(|c| c.node_id == node_id)
        {
            if bucket.replacement_candidates.len() >= K {
                bucket.replacement_candidates.remove(0);
            }
            bucket.replacement_candidates.push(candidate.clone());
        }

        if let Some((_, questionable)) = oldest_questionable {
            return AddNodeResult::PingQuestionable {
                questionable_node: questionable.clone(),
            };
        }

        AddNodeResult::Cached
    }

    /// Mark a node as failed (timed out).
    /// Returns the `NodeEntry` that was evicted and replaced (if any), along with its replacement.
    pub fn mark_failed(&mut self, node_id: &NodeId) -> Option<(NodeEntry, NodeEntry)> {
        let index = self.find_bucket_index(node_id);
        if index == 0 {
            return None;
        }

        let bucket = &mut self.buckets[index];
        if let Some(pos) = bucket.nodes.iter().position(|n| n.node_id == *node_id) {
            let entry = &mut bucket.nodes[pos];
            entry.timed_out();

            // If the node is now bad and we have replacement candidates, evict and promote
            if entry.is_bad() && !bucket.replacement_candidates.is_empty() {
                let evicted = bucket.nodes.remove(pos);
                let mut replacement = bucket.replacement_candidates.remove(0);
                replacement.status = ContactStatus::Confirmed {
                    consecutive_failures: 0,
                };
                replacement.last_seen = Some(Instant::now());
                bucket.nodes.push(replacement.clone());
                bucket.last_changed = Instant::now();
                return Some((evicted, replacement));
            }
        }
        None
    }

    /// Generate a random `NodeId` that falls into the bucket at the given index.
    pub fn random_id_in_bucket(&self, index: usize) -> NodeId {
        assert!(index > 0 && index < 160);
        let d = NodeId::random();
        let lz = 159 - index;
        let mut bytes = *d.as_bytes();

        // Zero out bits before lz
        for i in 0..lz {
            let byte_idx = i / 8;
            let bit_idx = i % 8;
            bytes[byte_idx] &= !(0x80 >> bit_idx);
        }

        // Set the lz-th bit to 1
        let byte_idx = lz / 8;
        let bit_idx = lz % 8;
        bytes[byte_idx] |= 0x80 >> bit_idx;

        let d_modified = NodeId::from_bytes(bytes);
        self.local_id ^ d_modified
    }

    pub fn find_closest(&self, target: &NodeId, count: usize) -> Vec<NodeEntry> {
        let mut all_nodes: Vec<NodeEntry> = self
            .buckets
            .iter()
            .flat_map(|b| b.nodes.iter().filter(|n| !n.is_bad()).cloned())
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

    /// Iterate over all nodes in the routing table, across every k-bucket.
    pub fn iter(&self) -> RoutingTableIter<'_> {
        RoutingTableIter {
            buckets: self.buckets.iter(),
            current: [].iter(),
        }
    }
}

/// Iterator over all `&NodeEntry` items in a `RoutingTable`.
///
/// Yields entries from each k-bucket in order of bucket index
/// (closest distance exponent first).
pub struct RoutingTableIter<'a> {
    buckets: std::slice::Iter<'a, KBucket>,
    current: std::slice::Iter<'a, NodeEntry>,
}

impl<'a> Iterator for RoutingTableIter<'a> {
    type Item = &'a NodeEntry;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.current.next() {
                return Some(entry);
            }
            let bucket = self.buckets.next()?;
            self.current = bucket.nodes.iter();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn test_random_id_in_bucket() {
        let local_id = NodeId::random();
        let table = RoutingTable::new_v4(local_id);

        for index in 1..160 {
            let target_id = table.random_id_in_bucket(index);
            let dist_exp = local_id.distance_exp(&target_id);
            assert_eq!(
                dist_exp, index,
                "Random ID at index {index} had unexpected distance exponent {dist_exp}"
            );
        }
    }

    #[test]
    fn test_add_node_bucket_flow() {
        let local_id = NodeId::random();
        let mut table = RoutingTable::new_v4(local_id);
        let addr: SocketAddr = "127.0.0.1:8000".parse().unwrap();

        // 1. Generate nodes that fall into the exact same bucket (e.g. index 150)
        let mut bucket_nodes = Vec::new();
        for _ in 0..K {
            let id = table.random_id_in_bucket(150);
            bucket_nodes.push(id);
            let res = table.add_node(id, addr, true);
            assert!(matches!(res, AddNodeResult::Added));
        }

        assert_eq!(table.buckets[150].nodes.len(), K);
        assert_eq!(table.buckets[150].replacement_candidates.len(), 0);

        // 2. Add K+1-th node, which should be cached since all existing are good/confirmed
        let extra_id = table.random_id_in_bucket(150);
        let res = table.add_node(extra_id, addr, true);
        assert!(matches!(res, AddNodeResult::Cached));
        assert_eq!(table.buckets[150].nodes.len(), K);
        assert_eq!(table.buckets[150].replacement_candidates.len(), 1);
        assert_eq!(
            table.buckets[150].replacement_candidates[0].node_id,
            extra_id
        );

        // 3. Mark one active node as failed twice (making it Bad)
        let failed_id = bucket_nodes[0];
        let evicted = table.mark_failed(&failed_id);

        // Since we have a replacement candidate (extra_id), marking failed_id as failed once
        // makes consecutive_failures = 1. It is not bad yet (needs >= 2).
        assert!(evicted.is_none());
        assert_eq!(table.buckets[150].nodes[0].fail_count(), 1);

        // Mark it failed again, it should become bad and get evicted, promoting the replacement candidate!
        let evicted = table.mark_failed(&failed_id);
        assert!(evicted.is_some());
        let (evicted_node, replacement_node) = evicted.unwrap();
        assert_eq!(evicted_node.node_id, failed_id);
        assert_eq!(replacement_node.node_id, extra_id);

        // Verify the routing table structure matches the eviction and promotion
        assert_eq!(table.buckets[150].nodes.len(), K);
        assert_eq!(table.buckets[150].replacement_candidates.len(), 0);
        assert!(
            table.buckets[150]
                .nodes
                .iter()
                .any(|n| n.node_id == extra_id)
        );
        assert!(
            !table.buckets[150]
                .nodes
                .iter()
                .any(|n| n.node_id == failed_id)
        );
    }

    #[test]
    fn test_ping_questionable_node() {
        let local_id = NodeId::random();
        let mut table = RoutingTable::new_v4(local_id);
        let addr: SocketAddr = "127.0.0.1:8000".parse().unwrap();

        // 1. Fill a bucket (index 150)
        let mut bucket_nodes = Vec::new();
        for _ in 0..K {
            let id = table.random_id_in_bucket(150);
            bucket_nodes.push(id);
            table.add_node(id, addr, true);
        }

        // 2. Make one node Questionable by setting its last_seen to > 15 minutes ago
        let questionable_pos = 2;
        let questionable_id = bucket_nodes[questionable_pos];
        table.buckets[150].nodes[questionable_pos].last_seen = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(20 * 60))
                .unwrap(),
        );
        assert!(table.buckets[150].nodes[questionable_pos].is_questionable());

        // 3. Adding a new node should now trigger a PingQuestionable result for that node
        let new_id = table.random_id_in_bucket(150);
        let res = table.add_node(new_id, addr, true);

        if let AddNodeResult::PingQuestionable { questionable_node } = res {
            assert_eq!(questionable_node.node_id, questionable_id);
        } else {
            panic!("Expected PingQuestionable, got {res:?}");
        }

        // Verify it was added to the replacement candidates
        assert_eq!(table.buckets[150].replacement_candidates.len(), 1);
        assert_eq!(table.buckets[150].replacement_candidates[0].node_id, new_id);
    }
}
