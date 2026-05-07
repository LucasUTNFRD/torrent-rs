use std::{
    array,
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
};
use tokio::time::Instant;

pub const NODE_ID_LEN: usize = 20;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct NodeId([u8; NODE_ID_LEN]);

impl NodeId {
    pub const fn from_bytes(b: [u8; 20]) -> Self {
        Self(b)
    }
    pub const fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
    pub const fn as_bytes(&self) -> [u8; 20] {
        self.0
    }

    fn leading_zeros(&self) -> u32 {
        let mut count = 0;
        for &byte in &self.0 {
            let lz = byte.leading_zeros();
            count += lz;
            if lz < 8 {
                break;
            }
        }
        count
    }

    /// Generates a random NodeId that is "close" to this one.
    /// It keeps most of the prefix and randomizes the last few bits.
    pub fn generate_neighbor(&self) -> Self {
        let mut bytes = self.0;
        // Keep the first 19 bytes, randomize the last one
        bytes[19] = rand::random::<u8>();
        Self(bytes)
    }
}

impl std::ops::BitXor for NodeId {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self::Output {
        Self(array::from_fn(|i| self.0[i] ^ rhs.0[i]))
    }
}

// Implementation for borrowed values
impl std::ops::BitXor<&NodeId> for &NodeId {
    type Output = NodeId;

    fn bitxor(self, rhs: &NodeId) -> Self::Output {
        NodeId(array::from_fn(|i| self.0[i] ^ rhs.0[i]))
    }
}

// A node is considered questionable if we haven't heard from it in 15 minutes.
// It is a candidate for eviction when the bucket is full.
const NODE_EXPIRY_SECS: u64 = 15 * 60;

// After this many unanswered pings a node is considered bad and will be evicted.
const MAX_UNANSWERED_PINGS: usize = 2;

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub addr: SocketAddr,

    /// Last time we received *any* message from this node (query or response).
    pub last_seen: Instant,
    /// Last time this node replied to one of our queries.
    pub last_replied: Instant,
    /// How many of our pings/queries this node has not answered yet.
    pub unanswered: usize,
}

impl Node {
    pub fn new(id: NodeId, addr: SocketAddr) -> Self {
        let now = Instant::now();

        Self {
            id,
            addr,
            last_seen: now,
            last_replied: now,
            unanswered: 0,
        }
    }
    // A good node has replied recently and has not accumulated too many
    /// unanswered pings.
    pub fn is_good(&self) -> bool {
        self.unanswered < MAX_UNANSWERED_PINGS
            && self.last_replied.elapsed().as_secs() < NODE_EXPIRY_SECS
    }

    /// A bad node is one we have repeatedly failed to reach.
    pub const fn is_bad(&self) -> bool {
        self.unanswered >= MAX_UNANSWERED_PINGS
    }
}

#[derive(Debug, Clone)]
pub struct Bucket {
    pub nodes: Vec<Node>,
}

// Kademlia K parameter: max nodes per bucket.
const K: usize = 8;

impl Bucket {
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(K),
        }
    }

    pub fn try_insert(&mut self, id: NodeId, addr: SocketAddr) -> InsertResult {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == id) {
            node.last_seen = Instant::now();
            return InsertResult::Updated;
        }

        if self.nodes.len() < K {
            self.nodes.push(Node::new(id, addr));
            return InsertResult::Inserted;
        }

        if let Some(pos) = self.nodes.iter().position(|n| n.is_bad()) {
            self.nodes.swap_remove(pos);
            self.nodes.push(Node::new(id, addr));
            return InsertResult::Inserted;
        }

        if let Some(_node) = self
            .nodes
            .iter()
            .find(|n| n.last_seen.elapsed().as_secs() >= NODE_EXPIRY_SECS)
        {
            // TODO: Signal that a ping is needed to verify this node's health
        }

        InsertResult::Dropped
    }
}

/// Kademlia routing table.
pub struct KTable {
    own_id: NodeId,
    buckets: [Bucket; 160],
}

impl KTable {
    pub fn new(own_id: NodeId) -> Self {
        Self {
            own_id,
            buckets: std::array::from_fn(|_| Bucket::new()),
        }
    }

    fn bucket_index(&self, id: NodeId) -> Option<usize> {
        let distance = self.own_id ^ id;
        let lz = distance.leading_zeros();
        if lz == 160 {
            None
        } else {
            Some(lz as usize)
        }
    }

    pub fn try_insert(&mut self, id: NodeId, addr: SocketAddr) -> Option<InsertResult> {
        let idx = self.bucket_index(id)?;
        Some(self.buckets[idx].try_insert(id, addr))
    }

    pub fn closest_nodes(&self, target: &NodeId, count: usize) -> Vec<(NodeId, SocketAddr)> {
        let mut all_nodes = Vec::new();
        for bucket in &self.buckets {
            for node in &bucket.nodes {
                if node.is_good() {
                    all_nodes.push((node.id, node.addr));
                }
            }
        }
        all_nodes.sort_by_key(|(id, _)| *id ^ *target);
        all_nodes.into_iter().take(count).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(|b| b.nodes.is_empty())
    }

    pub fn is_healthy(&self) -> bool {
        self.good_nodes_count() >= 8
    }

    pub fn good_nodes_count(&self) -> usize {
        self.buckets
            .iter()
            .map(|b| b.nodes.iter().filter(|n| n.is_good()).count())
            .sum()
    }
}

#[derive(Debug)]
pub enum InsertResult {
    Updated,
    Inserted,
    Dropped,
}
