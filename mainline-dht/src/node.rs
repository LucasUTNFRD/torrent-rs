use std::{net::SocketAddr, time::Instant};

use crate::node_id::NodeId;

/// A DHT node with its contact information and status.
#[derive(Debug, Clone)]
pub struct Node {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub status: NodeStatus,
    pub last_seen: Instant,
}

impl Node {
    /// Create a new node with the given ID and address.
    /// Initial status is Questionable until we receive a response.
    pub fn new(node_id: NodeId, addr: SocketAddr) -> Self {
        Self {
            node_id,
            addr,
            status: NodeStatus::Questionable,
            last_seen: Instant::now(),
        }
    }

    /// Create a new node marked as Good (just responded to us).
    pub fn new_good(node_id: NodeId, addr: SocketAddr) -> Self {
        Self {
            node_id,
            addr,
            status: NodeStatus::Good,
            last_seen: Instant::now(),
        }
    }

    /// Update the node's status and refresh last_seen timestamp.
    pub fn mark_good(&mut self) {
        self.status = NodeStatus::Good;
        self.last_seen = Instant::now();
    }

    /// Mark the node as having failed to respond.
    #[allow(dead_code)]
    pub fn mark_bad(&mut self) {
        self.status = NodeStatus::Bad;
    }

    /// Check if the node is considered good (responded recently).
    pub fn is_good(&self) -> bool {
        self.status == NodeStatus::Good
    }

    /// Check if the node should be considered questionable (not seen in 15+ minutes).
    pub fn is_questionable(&self) -> bool {
        self.status == NodeStatus::Questionable || self.last_seen.elapsed().as_secs() > 15 * 60
    }
}

/// Node status per BEP 0005.
/// - Good: Responded to our query within the last 15 minutes
/// - Questionable: Haven't heard from in 15+ minutes
/// - Bad: Failed to respond to multiple queries
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum NodeStatus {
    Good,
    Questionable,
    Bad,
}
