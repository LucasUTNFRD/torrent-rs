use std::{
    array,
    net::{SocketAddrV4, SocketAddrV6},
};
use tokio::time::Instant;

pub const NODE_ID_LEN: usize = 20;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
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

#[derive(Debug)]
pub struct Node<A> {
    pub id: NodeId,
    pub addr: A,

    /// Last time we received *any* message from this node (query or response).
    pub last_seen: Instant,
    /// Last time this node replied to one of our queries.
    pub last_replied: Instant,
    /// How many of our pings/queries this node has not answered yet.
    pub unanswered: usize,
}

impl<A> Node<A> {
    pub fn new(id: NodeId, addr: A) -> Self {
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
    pub fn is_bad(&self) -> bool {
        self.unanswered >= MAX_UNANSWERED_PINGS
    }
}

#[derive(Debug)]
pub struct Bucket<A> {
    pub nodes: Vec<Node<A>>,
}

// Kademlia K parameter: max nodes per bucket.
const K: usize = 8;

impl<A: Clone> Bucket<A> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(K),
        }
    }

    /// Try to insert a node.
    ///
    /// Returns `InsertResult` describing what happened.  The caller is
    /// responsible for acting on `InsertResult::NeedsPing` by sending a ping
    /// to the returned address and calling `on_timeout` / `on_reply`
    /// accordingly.
    pub fn try_insert(&mut self, id: NodeId, addr: A) -> InsertResult {
        // Already known — refresh timestamps and we're done.
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == id) {
            node.last_seen = Instant::now();
            return InsertResult::Updated;
        }

        // Bucket has room — insert unconditionally.
        if self.nodes.len() < K {
            self.nodes.push(Node::new(id, addr));
            return InsertResult::Inserted;
        }

        // Bucket is full.  Look for a bad node to evict first.
        if let Some(pos) = self.nodes.iter().position(|n| n.is_bad()) {
            self.nodes.swap_remove(pos);
            self.nodes.push(Node::new(id, addr));
            return InsertResult::Inserted;
        }

        // No bad nodes.  Look for a questionable node (not heard from recently).
        // We return its address so the caller can ping it.  If it turns out to
        // be dead the caller will call on_timeout, which eventually makes it
        // bad and evictable.
        if let Some(node) = self
            .nodes
            .iter()
            .find(|n| n.last_seen.elapsed().as_secs() >= NODE_EXPIRY_SECS)
        {
            todo!("Need ping");
            // return InsertResult::NeedsPing {
            //     candidate_id: id,
            //     candidate_addr: addr,
            //     stale_addr: node.addr.clone(),
            //     stale_id: node.id,
            // };
        }

        // All nodes are good and recently seen — drop the newcomer.
        InsertResult::Dropped
    }
}

/// Fixed-trie routing table.
///
/// 160 buckets indexed by the number of leading zero bits in
/// `own_id XOR candidate_id`.  Bucket 0 holds maximally distant nodes,
/// bucket 159 holds nodes that differ from us in only the last bit.
///
/// IPv4 and IPv6 are tracked in separate tables because the Kademlia ID space
/// is shared but the transport is not: a node may have both an IPv4 and an
/// IPv6 address but they are independent DHT participants.
pub struct RoutingTable {
    own_id: NodeId,
    v4: [Bucket<SocketAddrV4>; 160],
    v6: [Bucket<SocketAddrV6>; 160],
}

impl RoutingTable {
    pub fn new(own_id: NodeId) -> Self {
        Self {
            own_id,
            v4: std::array::from_fn(|_| Bucket::new()),
            v6: std::array::from_fn(|_| Bucket::new()),
        }
    }

    // ── index computation ────────────────────────────────────────────────────
    /// Map a node ID to a bucket index.
    ///
    /// Returns `None` if `id == own_id` (XOR distance == 0, no valid bucket).
    fn bucket_index(&self, id: NodeId) -> Option<usize> {
        let distance = self.own_id ^ id;
        let lz = distance.leading_zeros();
        if lz == 160 { None } else { Some(lz as usize) }
    }

    pub fn try_insert_v4(&mut self, id: NodeId, addr: SocketAddrV4) -> Option<InsertResult> {
        let idx = self.bucket_index(id)?;
        Some(self.v4[idx].try_insert(id, addr))
    }

    pub fn try_insert_v6(&mut self, id: NodeId, addr: SocketAddrV6) -> Option<InsertResult> {
        let idx = self.bucket_index(id)?;
        Some(self.v6[idx].try_insert(id, addr))
    }

    pub fn good_nodes_count(&self) -> usize {
        let mut count = 0;
        for bucket in &self.v4 {
            count += bucket.nodes.iter().filter(|n| n.is_good()).count();
        }
        for bucket in &self.v6 {
            count += bucket.nodes.iter().filter(|n| n.is_good()).count();
        }
        count
    }
}

#[derive(Debug)]
pub enum InsertResult {
    /// Node was already known; timestamps refreshed.
    Updated,
    /// Node was inserted (bucket had room or a bad node was evicted).
    Inserted,
    /// Bucket is full of genuinely good nodes; newcomer was dropped.
    Dropped,
    // NeedsPing {
    //     candidate_id: NodeId,
    //     candidate_addr: SocketAddr,
    //     stale_addr: SocketAddr,
    //     stale_id: NodeId,
    // },
}
