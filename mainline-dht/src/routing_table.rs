use std::time::Instant;

use crate::{node::Node, node_id::NodeId};

/// K is the maximum number of nodes per bucket (per BEP 0005).
pub const K: usize = 8;

/// Number of buckets covering the 160-bit key space.
const NUM_BUCKETS: usize = 160;

/// The routing table maintains K-buckets of known nodes.
/// Each bucket covers a portion of the 160-bit ID space based on XOR distance.
#[derive(Debug)]
pub struct RoutingTable {
    /// Our local node ID - used for distance calculations.
    local_node_id: NodeId,
    /// 160 buckets indexed by the bit length of XOR distance.
    buckets: Vec<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    /// Nodes in this bucket, ordered by last seen (oldest first).
    nodes: Vec<Node>,
    /// When this bucket was last modified.
    last_changed: Instant,
}

impl Bucket {
    fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(K),
            last_changed: Instant::now(),
        }
    }

    fn touch(&mut self) {
        self.last_changed = Instant::now();
    }
}

impl RoutingTable {
    /// Create a new routing table for the given local node ID.
    pub fn new(local_node_id: NodeId) -> Self {
        let buckets = (0..NUM_BUCKETS).map(|_| Bucket::new()).collect();
        Self {
            local_node_id,
            buckets,
        }
    }

    /// Get our local node ID.
    #[allow(dead_code)]
    pub fn local_id(&self) -> NodeId {
        self.local_node_id
    }

    /// Calculate which bucket a node ID belongs to.
    /// Returns None if the ID is identical to our local ID.
    fn bucket_index(&self, target: &NodeId) -> Option<usize> {
        let distance = *target ^ self.local_node_id;
        let bitlen = distance.bitlen();
        if bitlen == 0 {
            None // Same as our ID
        } else {
            Some(bitlen - 1)
        }
    }

    /// Try to add a node to the routing table.
    /// Returns true if the node was added or already exists.
    pub fn try_add_node(&mut self, node: Node) -> bool {
        let Some(index) = self.bucket_index(&node.node_id) else {
            return false; // Can't add ourselves
        };

        let bucket = &mut self.buckets[index];

        // If node already exists, move it to the end (most recently seen)
        if let Some(pos) = bucket.nodes.iter().position(|n| n.node_id == node.node_id) {
            let mut existing = bucket.nodes.remove(pos);
            existing.mark_good();
            bucket.nodes.push(existing);
            bucket.touch();
            return true;
        }

        // If bucket has space, add the node
        if bucket.nodes.len() < K {
            bucket.nodes.push(node);
            bucket.touch();
            return true;
        }

        // Bucket is full - in a full implementation we'd ping the LRU node
        // For now, we don't add the new node if the bucket is full
        // (This is a simplification; proper impl would ping oldest node)
        false
    }

    /// Update an existing node's status to Good.
    pub fn mark_node_good(&mut self, node_id: &NodeId) {
        let Some(index) = self.bucket_index(node_id) else {
            return;
        };

        let bucket = &mut self.buckets[index];
        if let Some(node) = bucket.nodes.iter_mut().find(|n| n.node_id == *node_id) {
            node.mark_good();
            bucket.touch();
        }
    }

    /// Get the K closest nodes to a target ID.
    pub fn get_closest_nodes(&self, target: &NodeId, k: usize) -> Vec<Node> {
        // Collect all nodes with their distances
        let mut nodes_with_distance: Vec<_> = self
            .buckets
            .iter()
            .flat_map(|bucket| bucket.nodes.iter())
            .filter(|node| node.is_good() || node.is_questionable())
            .map(|node| {
                let distance = node.node_id ^ *target;
                (distance, node.clone())
            })
            .collect();

        // Sort by XOR distance (smaller = closer)
        nodes_with_distance.sort_by(|a, b| {
            // Compare distance byte by byte
            let a_bytes = a.0.as_bytes();
            let b_bytes = b.0.as_bytes();
            a_bytes.cmp(&b_bytes)
        });

        // Return up to k closest
        nodes_with_distance
            .into_iter()
            .take(k)
            .map(|(_, node)| node)
            .collect()
    }

    /// Get all good nodes (for iteration/debugging).
    #[allow(dead_code)]
    pub fn get_good_nodes(&self) -> Vec<&Node> {
        self.buckets
            .iter()
            .flat_map(|bucket| bucket.nodes.iter())
            .filter(|node| node.is_good())
            .collect()
    }

    /// Total number of nodes in the routing table.
    pub fn node_count(&self) -> usize {
        self.buckets.iter().map(|b| b.nodes.len()).sum()
    }

    /// Number of good nodes in the routing table.
    #[allow(dead_code)]
    pub fn good_node_count(&self) -> usize {
        self.buckets
            .iter()
            .flat_map(|bucket| bucket.nodes.iter())
            .filter(|node| node.is_good())
            .count()
    }

    /// Check if the routing table is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.node_count() == 0
    }
}

#[cfg(test)]
mod test {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::{RoutingTable, K};
    use crate::{node::Node, node_id::NodeId};

    fn make_node(id_byte: u8) -> Node {
        let mut id_bytes = [0u8; 20];
        id_bytes[19] = id_byte;
        let node_id = NodeId::from_bytes(id_bytes);
        let addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6881);
        Node::new_good(node_id, addr)
    }

    #[test]
    fn bucket_index_uses_xor_distance() {
        let local = NodeId::from_bytes([0u8; 20]);
        let table = RoutingTable::new(local);

        // Node with high bit set -> furthest bucket (159)
        let far = NodeId::from_bytes([
            0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(table.bucket_index(&far), Some(159));

        // Node with only low bit set -> closest bucket (0)
        let mut near_bytes = [0u8; 20];
        near_bytes[19] = 1;
        let near = NodeId::from_bytes(near_bytes);
        assert_eq!(table.bucket_index(&near), Some(0));

        // Same as local -> None
        assert_eq!(table.bucket_index(&local), None);
    }

    #[test]
    fn add_nodes_to_bucket() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);

        // Create nodes that all fall in the same bucket (bucket 159 - highest bit set)
        // All have the pattern 0x80 in first byte, varying in last byte
        fn make_far_node(suffix: u8) -> Node {
            let mut id_bytes = [0u8; 20];
            id_bytes[0] = 0x80; // High bit set -> bucket 159
            id_bytes[19] = suffix;
            let node_id = NodeId::from_bytes(id_bytes);
            let addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6881 + suffix as u16);
            Node::new_good(node_id, addr)
        }

        // Add K nodes to the same bucket
        for i in 1..=K as u8 {
            let node = make_far_node(i);
            assert!(table.try_add_node(node), "Should add node {i}");
        }

        assert_eq!(table.node_count(), K);

        // Adding one more to the same bucket should fail (bucket full)
        let extra = make_far_node(K as u8 + 1);
        assert!(!table.try_add_node(extra), "Should reject when bucket full");
        assert_eq!(table.node_count(), K);
    }

    #[test]
    fn get_closest_nodes_returns_sorted() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);

        // Add nodes with IDs 1, 2, 3, 4, 5
        for i in 1..=5u8 {
            table.try_add_node(make_node(i));
        }

        // Target is 0, so nodes 1, 2, 3 are closest (XOR distance = their ID)
        let target = NodeId::from_bytes([0u8; 20]);
        let closest = table.get_closest_nodes(&target, 3);

        assert_eq!(closest.len(), 3);
        // Should be sorted by distance (1, 2, 3)
        assert_eq!(closest[0].node_id.as_bytes()[19], 1);
        assert_eq!(closest[1].node_id.as_bytes()[19], 2);
        assert_eq!(closest[2].node_id.as_bytes()[19], 3);
    }
}
