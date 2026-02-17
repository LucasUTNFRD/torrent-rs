//! Transaction manager for tracking in-flight DHT queries.
//!
//! This module provides a single-threaded, event-loop based transaction management
//! system. Unlike the await-based approach, this stores query context in transactions
//! and processes responses asynchronously when they arrive.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::atomic::{AtomicU16, Ordering},
    time::{Duration, Instant},
};

use bittorrent_common::types::InfoHash;

use crate::message::TransactionId;
use crate::node_id::NodeId;

/// Type of DHT query.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryType {
    Ping,
    FindNode {
        target: NodeId,
    },
    GetPeers {
        info_hash: InfoHash,
    },
    AnnouncePeer {
        info_hash: InfoHash,
        port: u16,
        implied_port: bool,
    },
}

impl QueryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueryType::Ping => "ping",
            QueryType::FindNode { .. } => "find_node",
            QueryType::GetPeers { .. } => "get_peers",
            QueryType::AnnouncePeer { .. } => "announce_peer",
        }
    }
}

/// A transaction that is pending a response.
#[derive(Debug, Clone)]
pub struct Transaction {
    /// The transaction ID used to correlate requests and responses.
    pub tx_id: TransactionId,
    /// The address of the node we sent the query to.
    pub addr: SocketAddr,
    /// The type of query and its context.
    pub query_type: QueryType,
    /// When this transaction was created.
    pub created_at: Instant,
    /// Number of retry attempts made.
    pub retries: u8,
    /// Optional: target for find_node queries
    pub target: Option<NodeId>,
    /// Optional: info_hash for get_peers/announce queries  
    pub info_hash: Option<InfoHash>,
    /// Optional: port for announce_peer queries
    pub announce_port: Option<u16>,
    /// Optional: implied_port flag
    pub implied_port: bool,
}

impl Transaction {
    /// Create a new transaction.
    pub fn new(tx_id: TransactionId, addr: SocketAddr, query_type: QueryType) -> Self {
        let mut tx = Self {
            tx_id,
            addr,
            query_type,
            created_at: Instant::now(),
            retries: 0,
            target: None,
            info_hash: None,
            announce_port: None,
            implied_port: false,
        };

        // Extract context from query type
        match &tx.query_type {
            QueryType::FindNode { target } => {
                tx.target = Some(*target);
            }
            QueryType::GetPeers { info_hash } => {
                tx.info_hash = Some(*info_hash);
            }
            QueryType::AnnouncePeer {
                info_hash,
                port,
                implied_port,
            } => {
                tx.info_hash = Some(*info_hash);
                tx.announce_port = Some(*port);
                tx.implied_port = *implied_port;
            }
            _ => {}
        }

        tx
    }

    /// Get the query type as a string.
    pub fn query_type_str(&self) -> &'static str {
        self.query_type.as_str()
    }

    /// Check if this transaction has timed out.
    pub fn is_timed_out(&self, timeout: Duration) -> bool {
        Instant::now().duration_since(self.created_at) > timeout
    }
}

/// Result of scanning for timeouts.
pub struct TimeoutScanResult {
    /// Transactions that should be retried.
    pub to_retry: Vec<Transaction>,
    /// Transaction IDs that exceeded max retries and should be removed.
    pub to_remove: Vec<TransactionId>,
}

/// Manages in-flight DHT transactions in a single-threaded context.
///
/// This manager is designed for event-loop based async processing where
/// responses are handled as they arrive rather than awaiting on each query.
pub struct TransactionManager {
    /// Counter for generating unique transaction IDs.
    next_tx_id: AtomicU16,
    /// Map from transaction ID to pending transaction.
    transactions: HashMap<u16, Transaction>,
    /// Index: query_type + addr -> transaction ID (prevents duplicate queries to same node)
    index: HashMap<String, u16>,
}

impl TransactionManager {
    /// Create a new transaction manager.
    pub fn new() -> Self {
        Self {
            next_tx_id: AtomicU16::new(0),
            transactions: HashMap::new(),
            index: HashMap::new(),
        }
    }

    /// Generate a new unique transaction ID.
    pub fn gen_id(&self) -> TransactionId {
        let id = self.next_tx_id.fetch_add(1, Ordering::Relaxed);
        TransactionId::new(id)
    }

    /// Create index key for preventing duplicate queries.
    fn make_index_key(&self, query_type: &str, addr: &SocketAddr) -> String {
        format!("{}:{}", query_type, addr)
    }

    /// Insert a transaction into the manager.
    ///
    /// Returns true if inserted, false if a transaction with the same
    /// query type and address already exists (prevents duplicates).
    pub fn insert(&mut self, tx: Transaction) -> bool {
        let tx_id_num = u16::from_be_bytes(tx.tx_id.0);
        let index_key = self.make_index_key(tx.query_type_str(), &tx.addr);

        // Check for duplicate
        if self.index.contains_key(&index_key) {
            return false;
        }

        self.index.insert(index_key, tx_id_num);
        self.transactions.insert(tx_id_num, tx);
        true
    }

    /// Get a transaction by its transaction ID.
    pub fn get_by_trans_id(&self, tx_id: &TransactionId) -> Option<&Transaction> {
        self.transactions.get(&u16::from_be_bytes(tx_id.0))
    }

    /// Get a mutable transaction by its transaction ID.
    pub fn get_by_trans_id_mut(&mut self, tx_id: &TransactionId) -> Option<&mut Transaction> {
        self.transactions.get_mut(&u16::from_be_bytes(tx_id.0))
    }

    /// Get a transaction by query type and address (using index).
    pub fn get_by_index(&self, query_type: &str, addr: &SocketAddr) -> Option<&Transaction> {
        let index_key = self.make_index_key(query_type, addr);
        self.index
            .get(&index_key)
            .and_then(|tx_id| self.transactions.get(tx_id))
    }

    /// Mark a transaction for retry (increment retry count and update timestamp).
    pub fn mark_retry(&mut self, tx_id: &TransactionId) -> bool {
        if let Some(tx) = self.transactions.get_mut(&u16::from_be_bytes(tx_id.0)) {
            tx.retries += 1;
            tx.created_at = Instant::now();
            true
        } else {
            false
        }
    }

    /// Finish (remove) a transaction by its ID.
    ///
    /// Returns true if a transaction was removed.
    pub fn finish_by_trans_id(&mut self, tx_id: &TransactionId) -> bool {
        let tx_id_num = u16::from_be_bytes(tx_id.0);
        if let Some(tx) = self.transactions.remove(&tx_id_num) {
            let index_key = self.make_index_key(tx.query_type_str(), &tx.addr);
            self.index.remove(&index_key);
            true
        } else {
            false
        }
    }

    /// Scan for timed-out transactions.
    ///
    /// Returns transactions that should be retried (under max_retries)
    /// and transaction IDs that should be removed (exceeded max_retries).
    pub fn scan_timeouts(&mut self, timeout: Duration, max_retries: u8) -> TimeoutScanResult {
        let now = Instant::now();
        let mut to_retry = Vec::new();
        let mut to_remove = Vec::new();
        let mut remove_ids = Vec::new();

        for (tx_id_num, tx) in &self.transactions {
            if now.duration_since(tx.created_at) > timeout {
                if tx.retries < max_retries {
                    to_retry.push(Transaction {
                        tx_id: tx.tx_id.clone(),
                        addr: tx.addr,
                        query_type: tx.query_type.clone(),
                        created_at: tx.created_at,
                        retries: tx.retries,
                        target: tx.target,
                        info_hash: tx.info_hash,
                        announce_port: tx.announce_port,
                        implied_port: tx.implied_port,
                    });
                } else {
                    to_remove.push(TransactionId::new(*tx_id_num));
                    remove_ids.push(*tx_id_num);
                }
            }
        }

        // Remove timed-out transactions that exceeded max retries
        for tx_id_num in remove_ids {
            if let Some(tx) = self.transactions.remove(&tx_id_num) {
                let index_key = self.make_index_key(tx.query_type_str(), &tx.addr);
                self.index.remove(&index_key);
            }
        }

        TimeoutScanResult {
            to_retry,
            to_remove,
        }
    }

    /// Get the number of pending transactions.
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    /// Check if there are no pending transactions.
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }

    /// Clear all transactions.
    pub fn clear(&mut self) {
        self.transactions.clear();
        self.index.clear();
    }
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn test_insert_and_get() {
        let mut manager = TransactionManager::new();
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 6881));
        let tx_id = manager.gen_id();

        let tx = Transaction::new(tx_id.clone(), addr, QueryType::Ping);
        assert!(manager.insert(tx));

        // Should be able to retrieve by ID
        let retrieved = manager.get_by_trans_id(&tx_id);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().addr, addr);

        // Should be able to retrieve by index
        let by_index = manager.get_by_index("ping", &addr);
        assert!(by_index.is_some());
    }

    #[test]
    fn test_prevent_duplicate() {
        let mut manager = TransactionManager::new();
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 6881));

        let tx1 = Transaction::new(manager.gen_id(), addr, QueryType::Ping);
        assert!(manager.insert(tx1));

        // Second ping to same address should be rejected
        let tx2 = Transaction::new(manager.gen_id(), addr, QueryType::Ping);
        assert!(!manager.insert(tx2));

        // But a find_node to same address should be allowed
        let target = NodeId::generate_random();
        let tx3 = Transaction::new(manager.gen_id(), addr, QueryType::FindNode { target });
        assert!(manager.insert(tx3));
    }

    #[test]
    fn test_finish_transaction() {
        let mut manager = TransactionManager::new();
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 6881));
        let tx_id = manager.gen_id();

        let tx = Transaction::new(tx_id.clone(), addr, QueryType::Ping);
        manager.insert(tx);
        assert_eq!(manager.len(), 1);

        // Finish the transaction
        assert!(manager.finish_by_trans_id(&tx_id));
        assert_eq!(manager.len(), 0);

        // Should not be able to retrieve anymore
        assert!(manager.get_by_trans_id(&tx_id).is_none());
    }

    #[test]
    fn test_scan_timeouts() {
        let mut manager = TransactionManager::new();
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 6881));

        // Insert a transaction
        let tx_id = manager.gen_id();
        let tx = Transaction::new(tx_id.clone(), addr, QueryType::Ping);
        manager.insert(tx);

        // Scan with 0 timeout should mark it for retry (0 retries < max 2)
        let result = manager.scan_timeouts(Duration::from_secs(0), 2);
        assert_eq!(result.to_retry.len(), 1);
        assert_eq!(result.to_remove.len(), 0);
        // Transaction still exists for retry
        assert_eq!(manager.len(), 1);

        // Mark as retried twice
        manager.mark_retry(&tx_id);
        manager.mark_retry(&tx_id);

        // Now scan should mark for removal (2 retries >= max 2)
        let result = manager.scan_timeouts(Duration::from_secs(0), 2);
        assert_eq!(result.to_retry.len(), 0);
        assert_eq!(result.to_remove.len(), 1);
        // Transaction should be removed
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_transaction_context() {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 6881));
        let info_hash = InfoHash::new([0xab; 20]);

        let tx = Transaction::new(
            TransactionId::new(1),
            addr,
            QueryType::GetPeers { info_hash },
        );

        assert_eq!(tx.query_type_str(), "get_peers");
        assert_eq!(tx.info_hash, Some(info_hash));
    }
}
