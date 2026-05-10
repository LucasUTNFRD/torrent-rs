use crate::node_id::NodeId;
use std::net::SocketAddr;

struct NodeEntry {
    node_id: NodeId,
    addr: SocketAddr,
    is_verified: bool,
}

struct KBucket {
    pub nodes: [NodeEntry; 8],
    pub replacement_candidates: Vec<NodeEntry>,
}

struct RoutingTable {
    rt: [KBucket; 160],
}
