use std::{net::SocketAddr, time::Instant};

use crate::node_id::NodeId;

/// 15 minutes in seconds
const MIN_15: u64 = 15 * 60;

/// A DHT node with its contact information and status.
#[derive(Debug, Clone)]
pub struct Node {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub status: NodeStatus,
    /// Last time this node responded to one of our queries
    pub last_response: Instant,
    /// Last time this node sent us a query
    pub last_query_from: Option<Instant>,
    /// Number of consecutive failed queries (reset on successfulresponse)
    pub failed_queries: u32,
}

impl Node {
    /// Create a new node with the given ID and address.
    /// Initial status is Questionable until we receive a response.
    pub fn new(node_id: NodeId, addr: SocketAddr) -> Self {
        Self {
            node_id,
            addr,
            status: NodeStatus::Questionable,
            last_response: Instant::now(),
            last_query_from: None,
            failed_queries: 0,
        }
    }

    /// Create a new node marked as Good (just responded to us).
    pub fn new_good(node_id: NodeId, addr: SocketAddr) -> Self {
        Self {
            node_id,
            addr,
            status: NodeStatus::Good,
            last_response: Instant::now(),
            last_query_from: None,
            failed_queries: 0,
        }
    }

    /// Update when the node successfully responds to one of our queries.
    pub fn mark_response(&mut self) {
        self.status = NodeStatus::Good;
        self.last_response = Instant::now();
        self.failed_queries = 0;
    }

    /// Update when we receive a query from this node.
    pub fn mark_query_from(&mut self) {
        self.last_query_from = Some(Instant::now());
    }

    /// Mark the node as having failed to respond to a query.
    #[allow(dead_code)]
    pub fn mark_failed(&mut self) {
        self.failed_queries += 1;
        if self.failed_queries >= 3 {
            self.status = NodeStatus::Bad;
        }
    }

    /// Check if the node is considered good per BEP 05:
    /// - Responded to one of our queries within the last 15 minutes, OR
    /// - Has ever responded AND sent us a query within the last 15 minutes
    pub fn is_good(&self) -> bool {
        let now = Instant::now();
        let responded_recently = self.status == NodeStatus::Good
            && now.duration_since(self.last_response).as_secs() < MIN_15;
        let heard_recently = self
            .last_query_from
            .map(|t| now.duration_since(t).as_secs() < MIN_15)
            .unwrap_or(false);

        responded_recently || (self.status == NodeStatus::Good && heard_recently)
    }

    /// Check if the node is questionable (not seen in 15+ minutes, but not bad).
    pub fn is_questionable(&self) -> bool {
        self.status != NodeStatus::Bad
            && Instant::now().duration_since(self.last_response).as_secs() > MIN_15
    }

    /// Check if the node is bad (failed multiple queries).
    #[allow(dead_code)]
    pub fn is_bad(&self) -> bool {
        self.status == NodeStatus::Bad || self.failed_queries >= 3
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
