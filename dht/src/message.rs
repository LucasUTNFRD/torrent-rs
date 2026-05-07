//! KRPC Protocol message types for BitTorrent DHT (BEP 0005).
//!
//! The KRPC protocol uses bencoded dictionaries over UDP with three message types:
//! - Query (y = "q"): Request from one node to another
//! - Response (y = "r"): Successful reply to a query  
//! - Error (y = "e"): Error reply to a query

use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
};

use bencode::{Bencode, BencodeBuilder, BencodeDict};
use bittorrent_common::types::InfoHash;

use crate::{error::DhtError, routing_table::NodeId};

///Transaction ID for correlating requests and responses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransactionId(pub [u8; 2]);

impl TransactionId {
    pub fn new(id: u16) -> Self {
        Self(id.to_be_bytes())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A complete KRPC message with common fields and body.
#[derive(Debug)]
pub struct KrpcMessage {
    /// Transaction ID for request/response correlation.
    pub transaction_id: TransactionId,
    /// Client version string (optional, 4 bytes: 2-char client ID + 2-char version).
    // TODO: use [u8;VERSION_STRING_LEN]
    pub version: Option<Vec<u8>>,
    /// Sender's external IP as seen by responder (BEP 42).
    pub sender_ip: Option<SocketAddr>,
    /// The message body (query, response, or error).
    pub body: MessageBody,
}

/// The body of a KRPC message.
#[derive(Debug)]
pub enum MessageBody {
    /// A query requesting an action.
    Query(Query),
    /// A successful response to a query.
    Response(Response),
    /// An error response.
    Error { code: i64, message: String },
}

/// DHT query types per BEP 5.
#[derive(Debug)]
pub enum Query {
    Ping {
        id: NodeId,
    },
    FindNode {
        id: NodeId,
        target: NodeId,
        is_bootstrap: bool,
    },
    GetPeers {
        id: NodeId,
        info_hash: InfoHash,
        is_bootstrap: bool,
    },
    AnnouncePeer {
        id: NodeId,
        info_hash: InfoHash,
        port: u16,
        token: Vec<u8>,
        /// If true, the source port of the UDP packet should be used as the peer's port.
        implied_port: bool,
    },
}

/// BEP 32: which address families the querier wants back in the reply.
#[derive(Debug, Clone, Copy, Default)]
pub struct Want {
    pub n4: bool,
    pub n6: bool,
}

impl Want {
    pub const fn v4_only() -> Self {
        Self {
            n4: true,
            n6: false,
        }
    }
    pub const fn v6_only() -> Self {
        Self {
            n4: false,
            n6: true,
        }
    }
    pub const fn both() -> Self {
        Self { n4: true, n6: true }
    }
}

// TODO: Change Query::FindNode and Query::GetPeers to carry want

/// DHT response types.
#[derive(Debug)]
pub enum Response {
    Ping {
        id: NodeId,
    },
    FindNode {
        id: NodeId,
        nodes: Vec<CompactNodeInfo>,
    },
    GetPeers {
        id: NodeId,
        /// Opaque write token for future announce_peer.
        token: Vec<u8>,
        /// Peers for the info_hash (if we have them).
        values: Option<Vec<SocketAddr>>,
        /// Closer nodes (if we don't have peers).
        nodes: Option<Vec<CompactNodeInfo>>,
    },
    /// Response to announce_peer (simple acknowledgment).
    AnnouncePeer {
        id: NodeId,
    },
}

/// Compact node info: 20-byte node ID + 6-byte IP:port (IPv4) or 18-byte IP:port (IPv6).
#[derive(Debug, Clone)]
pub struct CompactNodeInfo {
    pub node_id: NodeId,
    pub addr: SocketAddr,
}

// ============================================================================
// Encoding
// ============================================================================

impl KrpcMessage {
    /// Encode this message to bencoded bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut dict = BTreeMap::<Vec<u8>, Bencode>::new();

        // Transaction ID (required)
        dict.put("t", &self.transaction_id.0.as_slice());

        // Version (optional)
        if let Some(ref v) = self.version {
            dict.put("v", &v.as_slice());
        }

        // Message type and body
        match &self.body {
            MessageBody::Query(query) => {
                dict.put("y", &"q");
                match query {
                    Query::Ping { id } => {
                        dict.put("q", &"ping");
                        let mut args = BTreeMap::<Vec<u8>, Bencode>::new();
                        args.put("id", &id.as_slice());
                        dict.insert(b"a".to_vec(), args.build());
                    }
                    Query::FindNode {
                        id,
                        target,
                        is_bootstrap,
                    } => {
                        dict.put("q", &"find_node");
                        let mut args = BTreeMap::<Vec<u8>, Bencode>::new();
                        args.put("id", &id.as_slice());
                        args.put("target", &target.as_slice());
                        if *is_bootstrap {
                            args.insert(b"bs".to_vec(), Bencode::Int(1));
                        }
                        dict.insert(b"a".to_vec(), args.build());
                    }
                    Query::GetPeers {
                        id,
                        info_hash,
                        is_bootstrap,
                    } => {
                        dict.put("q", &"get_peers");
                        let mut args = BTreeMap::<Vec<u8>, Bencode>::new();
                        args.put("id", &id.as_slice());
                        args.put("info_hash", &info_hash.as_slice());
                        if *is_bootstrap {
                            args.insert(b"bs".to_vec(), Bencode::Int(1));
                        }
                        dict.insert(b"a".to_vec(), args.build());
                    }
                    Query::AnnouncePeer {
                        id,
                        info_hash,
                        port,
                        token,
                        implied_port,
                    } => {
                        dict.put("q", &"announce_peer");
                        let mut args = BTreeMap::<Vec<u8>, Bencode>::new();
                        args.put("id", &id.as_slice());
                        args.put("info_hash", &info_hash.as_slice());
                        args.insert(b"port".to_vec(), Bencode::Int(*port as i64));
                        args.put("token", &token.as_slice());
                        if *implied_port {
                            args.insert(b"implied_port".to_vec(), Bencode::Int(1));
                        }
                        dict.insert(b"a".to_vec(), args.build());
                    }
                }
            }
            MessageBody::Response(response) => {
                dict.put("y", &"r");
                let mut r = BTreeMap::<Vec<u8>, Bencode>::new();
                match response {
                    Response::Ping { id } => {
                        r.put("id", &id.as_slice());
                    }
                    Response::FindNode { id, nodes } => {
                        r.put("id", &id.as_slice());
                        let compact = encode_compact_nodes_v4(nodes);
                        r.put("nodes", &compact.as_slice());
                    }
                    Response::GetPeers {
                        id,
                        token,
                        values,
                        nodes,
                    } => {
                        r.put("id", &id.as_slice());
                        r.put("token", &token.as_slice());
                        // BEP 5: "values" is a list of strings, each being compact peer info
                        if let Some(peers) = values {
                            let values_list: Vec<Bencode> = peers
                                .iter()
                                .map(|p| Bencode::Bytes(encode_compact_peer(p)))
                                .collect();
                            r.insert(b"values".to_vec(), Bencode::List(values_list));
                        }
                        if let Some(nodes) = nodes {
                            let compact = encode_compact_nodes_v4(nodes);
                            r.put("nodes", &compact.as_slice());
                        }
                    }
                    Response::AnnouncePeer { id } => {
                        r.put("id", &id.as_slice());
                    }
                }
                dict.insert(b"r".to_vec(), r.build());
            }
            MessageBody::Error { code, message } => {
                dict.put("y", &"e");
                let error_list = Bencode::List(vec![
                    Bencode::Int(*code),
                    Bencode::Bytes(message.as_bytes().to_vec()),
                ]);
                dict.insert(b"e".to_vec(), error_list);
            }
        }

        Bencode::encoder(&dict.build())
    }

    /// Decode a KRPC message from bencoded bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DhtError> {
        let bencode = Bencode::decode(bytes)?;
        let Bencode::Dict(dict) = bencode else {
            return Err(DhtError::Parse("expected dictionary".to_string()));
        };

        // Transaction ID (required)
        let transaction_id = dict
            .get_bytes(b"t")
            .ok_or_else(|| DhtError::Parse("missing transaction id".to_string()))?;
        let transaction_id = match transaction_id.try_into() as Result<[u8; 2], _> {
            Ok(arr) => TransactionId(arr),
            Err(_) => {
                tracing::debug!("Invalid transaction ID length: {}", transaction_id.len());
                return Err(DhtError::Parse("invalid transaction id length".to_string()));
            }
        };

        // Version (optional)
        let version = dict.get_bytes(b"v").map(|v| v.to_vec());

        // Sender IP (optional, BEP 42)
        let sender_ip = dict.get_bytes(b"ip").and_then(|ip_bytes| {
            if ip_bytes.len() == 6 {
                Some(SocketAddr::V4(decode_compact_addr_v4(ip_bytes)))
            } else if ip_bytes.len() == 18 {
                decode_compact_addr_v6(ip_bytes)
            } else {
                None
            }
        });

        // Message type
        let msg_type = dict
            .get_str(b"y")
            .ok_or_else(|| DhtError::Parse("missing message type".to_string()))?;

        let body = match msg_type {
            "q" => parse_query(&dict)?,
            "r" => parse_response(&dict)?,
            "e" => parse_error(&dict)?,
            _ => return Err(DhtError::Parse(format!("unknown message type: {msg_type}"))),
        };

        Ok(KrpcMessage {
            transaction_id,
            version,
            sender_ip,
            body,
        })
    }

    /// Create a ping query message.
    pub fn ping_query(tx_id: u16, node_id: NodeId) -> Self {
        Self {
            transaction_id: TransactionId::new(tx_id),
            version: None,
            sender_ip: None,
            body: MessageBody::Query(Query::Ping { id: node_id }),
        }
    }

    /// Create a ping response message.
    pub fn ping_response(tx_id: TransactionId, node_id: NodeId) -> Self {
        Self {
            transaction_id: tx_id,
            version: None,
            sender_ip: None,
            body: MessageBody::Response(Response::Ping { id: node_id }),
        }
    }

    /// Create a find_node query message.
    pub fn find_node_query(tx_id: u16, node_id: NodeId, target: NodeId, is_bootstrap: bool) -> Self {
        Self {
            transaction_id: TransactionId::new(tx_id),
            version: None,
            sender_ip: None,
            body: MessageBody::Query(Query::FindNode {
                id: node_id,
                target,
                is_bootstrap,
            }),
        }
    }

    /// Create a find_node response message.
    pub fn find_node_response(
        tx_id: TransactionId,
        node_id: NodeId,
        nodes: Vec<CompactNodeInfo>,
    ) -> Self {
        Self {
            transaction_id: tx_id,
            version: None,
            sender_ip: None,
            body: MessageBody::Response(Response::FindNode { id: node_id, nodes }),
        }
    }

    /// Create an error response message.
    pub fn error_response(tx_id: TransactionId, code: i64, message: String) -> Self {
        Self {
            transaction_id: tx_id,
            version: None,
            sender_ip: None,
            body: MessageBody::Error { code, message },
        }
    }

    /// Create a get_peers query message.
    pub fn get_peers_query(
        tx_id: u16,
        node_id: NodeId,
        info_hash: InfoHash,
        is_bootstrap: bool,
    ) -> Self {
        Self {
            transaction_id: TransactionId::new(tx_id),
            version: None,
            sender_ip: None,
            body: MessageBody::Query(Query::GetPeers {
                id: node_id,
                info_hash,
                is_bootstrap,
            }),
        }
    }

    /// Create a get_peers response with peer values.
    pub fn get_peers_response_with_values(
        tx_id: TransactionId,
        node_id: NodeId,
        token: Vec<u8>,
        peers: Vec<SocketAddr>,
    ) -> Self {
        Self {
            transaction_id: tx_id,
            version: None,
            sender_ip: None,
            body: MessageBody::Response(Response::GetPeers {
                id: node_id,
                token,
                values: Some(peers),
                nodes: None,
            }),
        }
    }

    /// Create a get_peers response with closer nodes (no peers available).
    pub fn get_peers_response_with_nodes(
        tx_id: TransactionId,
        node_id: NodeId,
        token: Vec<u8>,
        nodes: Vec<CompactNodeInfo>,
    ) -> Self {
        Self {
            transaction_id: tx_id,
            version: None,
            sender_ip: None,
            body: MessageBody::Response(Response::GetPeers {
                id: node_id,
                token,
                values: None,
                nodes: Some(nodes),
            }),
        }
    }

    /// Create an announce_peer query message.
    pub fn announce_peer_query(
        tx_id: u16,
        node_id: NodeId,
        info_hash: InfoHash,
        port: u16,
        token: Vec<u8>,
        implied_port: bool,
    ) -> Self {
        Self {
            transaction_id: TransactionId::new(tx_id),
            version: None,
            sender_ip: None,
            body: MessageBody::Query(Query::AnnouncePeer {
                id: node_id,
                info_hash,
                port,
                token,
                implied_port,
            }),
        }
    }

    /// Create an announce_peer response message.
    pub fn announce_peer_response(tx_id: TransactionId, node_id: NodeId) -> Self {
        Self {
            transaction_id: tx_id,
            version: None,
            sender_ip: None,
            body: MessageBody::Response(Response::AnnouncePeer { id: node_id }),
        }
    }

    /// Get the node ID from this message (if it's a response).
    pub fn get_node_id(&self) -> Option<NodeId> {
        match &self.body {
            MessageBody::Response(Response::Ping { id }) => Some(*id),
            MessageBody::Response(Response::FindNode { id, .. }) => Some(*id),
            MessageBody::Response(Response::GetPeers { id, .. }) => Some(*id),
            MessageBody::Response(Response::AnnouncePeer { id }) => Some(*id),
            _ => None,
        }
    }

    /// Check if this is a response message.
    pub fn is_response(&self) -> bool {
        matches!(self.body, MessageBody::Response(_))
    }

    /// Check if this is a query message.
    pub fn is_query(&self) -> bool {
        matches!(self.body, MessageBody::Query(_))
    }
}

// ============================================================================
// Parsing helpers
// ============================================================================

fn parse_query(dict: &BTreeMap<Vec<u8>, Bencode>) -> Result<MessageBody, DhtError> {
    let method = dict
        .get_str(b"q")
        .ok_or_else(|| DhtError::Parse("missing query method".to_string()))?;

    let args = dict
        .get_dict(b"a")
        .ok_or_else(|| DhtError::Parse("missing query arguments".to_string()))?;

    let id = parse_node_id(args, b"id")?;

    let is_bootstrap = args.get_i64(b"bs").map(|v| v != 0).unwrap_or(false);

    match method {
        "ping" => Ok(MessageBody::Query(Query::Ping { id })),
        "find_node" => {
            let target = parse_node_id(args, b"target")?;
            Ok(MessageBody::Query(Query::FindNode {
                id,
                target,
                is_bootstrap,
            }))
        }
        "get_peers" => {
            let info_hash_bytes = args
                .get_bytes(b"info_hash")
                .ok_or_else(|| DhtError::Parse("missing info_hash".to_string()))?;
            if info_hash_bytes.len() != 20 {
                return Err(DhtError::Parse(format!(
                    "invalid info_hash length: expected 20, got {}",
                    info_hash_bytes.len()
                )));
            }
            let info_hash = InfoHash::from_slice(info_hash_bytes)
                .ok_or_else(|| DhtError::Parse("invalid info_hash".to_string()))?;
            Ok(MessageBody::Query(Query::GetPeers {
                id,
                info_hash,
                is_bootstrap,
            }))
        }
        "announce_peer" => {
            let info_hash_bytes = args
                .get_bytes(b"info_hash")
                .ok_or_else(|| DhtError::Parse("missing info_hash".to_string()))?;
            if info_hash_bytes.len() != 20 {
                return Err(DhtError::Parse(format!(
                    "invalid info_hash length: expected 20, got {}",
                    info_hash_bytes.len()
                )));
            }
            let info_hash = InfoHash::from_slice(info_hash_bytes)
                .ok_or_else(|| DhtError::Parse("invalid info_hash".to_string()))?;

            let port = args
                .get_i64(b"port")
                .ok_or_else(|| DhtError::Parse("missing port".to_string()))?
                as u16;

            let token = args
                .get_bytes(b"token")
                .ok_or_else(|| DhtError::Parse("missing token".to_string()))?
                .to_vec();

            // implied_port is optional, defaults to false (0)
            let implied_port = args
                .get_i64(b"implied_port")
                .map(|v| v != 0)
                .unwrap_or(false);

            Ok(MessageBody::Query(Query::AnnouncePeer {
                id,
                info_hash,
                port,
                token,
                implied_port,
            }))
        }
        _ => Err(DhtError::Parse(format!("unknown query method: {method}"))),
    }
}

fn parse_response(dict: &BTreeMap<Vec<u8>, Bencode>) -> Result<MessageBody, DhtError> {
    let r = dict
        .get_dict(b"r")
        .ok_or_else(|| DhtError::Parse("missing response body".to_string()))?;

    let id = parse_node_id(r, b"id")?;

    // Collect nodes from both "nodes" and "nodes6" fields if present.
    let mut nodes = Vec::new();
    if let Some(nodes_bytes) = r.get_bytes(b"nodes") {
        nodes.extend(decode_compact_nodes(nodes_bytes)?);
    }
    if let Some(nodes_bytes) = r.get_bytes(b"nodes6") {
        nodes.extend(decode_compact_nodes(nodes_bytes)?);
    }

    // Check for token → this is a get_peers response
    if let Some(token) = r.get_bytes(b"token") {
        let token = token.to_vec();

        // Parse values (list of compact peer info strings)
        let values = if let Some(Bencode::List(list)) = r.get(b"values".as_slice()) {
            let mut peers: Vec<SocketAddr> = Vec::new();
            for b in list.iter() {
                match b {
                    Bencode::Bytes(bytes) if bytes.len() == 6 => {
                        peers.push(SocketAddr::V4(decode_compact_addr_v4(bytes)));
                    }
                    Bencode::Bytes(bytes) if bytes.len() == 18 => {
                        if let Some(addr) = decode_compact_addr_v6(bytes) {
                            peers.push(addr);
                        }
                    }
                    _ => {}
                }
            }
            if peers.is_empty() {
                None
            } else {
                Some(peers)
            }
        } else {
            None
        };

        return Ok(MessageBody::Response(Response::GetPeers {
            id,
            token,
            values,
            nodes: if nodes.is_empty() { None } else { Some(nodes) },
        }));
    }

    // Check for nodes → FindNode response
    if !nodes.is_empty() {
        return Ok(MessageBody::Response(Response::FindNode { id, nodes }));
    }

    // Default: Ping response
    Ok(MessageBody::Response(Response::Ping { id }))
}

fn parse_error(dict: &BTreeMap<Vec<u8>, Bencode>) -> Result<MessageBody, DhtError> {
    let error_list = dict
        .get(b"e".as_slice())
        .ok_or_else(|| DhtError::Parse("missing error body".to_string()))?;

    let Bencode::List(list) = error_list else {
        return Err(DhtError::Parse("error body must be a list".to_string()));
    };

    if list.len() < 2 {
        return Err(DhtError::Parse(
            "error list must have 2 elements".to_string(),
        ));
    }

    let code = match &list[0] {
        Bencode::Int(i) => *i,
        _ => return Err(DhtError::Parse("error code must be integer".to_string())),
    };

    let message = match &list[1] {
        Bencode::Bytes(b) => String::from_utf8_lossy(b).to_string(),
        _ => return Err(DhtError::Parse("error message must be string".to_string())),
    };

    Ok(MessageBody::Error { code, message })
}

fn parse_node_id(dict: &BTreeMap<Vec<u8>, Bencode>, key: &[u8]) -> Result<NodeId, DhtError> {
    let bytes = dict
        .get_bytes(key)
        .ok_or_else(|| DhtError::Parse(format!("missing key: {}", String::from_utf8_lossy(key))))?;

    if bytes.len() != 20 {
        return Err(DhtError::Parse(format!(
            "invalid node ID length: expected 20, got {}",
            bytes.len()
        )));
    }

    let mut id = [0u8; 20];
    id.copy_from_slice(bytes);
    Ok(NodeId::from_bytes(id))
}

// ============================================================================
// Compact encoding for nodes (26 bytes each: 20-byte ID + 4-byte IP + 2-byte port for IPv4)
// ============================================================================

/// Encode a list of nodes to compact format (26 bytes per node for IPv4).
pub fn encode_compact_nodes_v4(nodes: &[CompactNodeInfo]) -> Vec<u8> {
    let mut result = Vec::with_capacity(nodes.len() * 26);
    for node in nodes {
        if let SocketAddr::V4(v4) = node.addr {
            result.extend_from_slice(&node.node_id.as_bytes());
            result.extend_from_slice(&v4.ip().octets());
            result.extend_from_slice(&v4.port().to_be_bytes());
        }
    }
    result
}

/// Encode a list of nodes to compact format (supports both IPv4 and IPv6).
pub fn encode_compact_nodes(nodes: &[CompactNodeInfo]) -> Vec<u8> {
    let mut result = Vec::new();
    for node in nodes {
        result.extend_from_slice(&node.node_id.as_bytes());
        result.extend_from_slice(&encode_compact_peer(&node.addr));
    }
    result
}

/// Decode compact node info (26 bytes per node for IPv4).
pub fn decode_compact_nodes_v4(data: &[u8]) -> Result<Vec<CompactNodeInfo>, DhtError> {
    if data.len() % 26 != 0 {
        return Err(DhtError::Parse(format!(
            "compact nodes length {} not divisible by 26",
            data.len()
        )));
    }

    let mut nodes = Vec::with_capacity(data.len() / 26);
    for chunk in data.chunks_exact(26) {
        let mut id_bytes = [0u8; 20];
        id_bytes.copy_from_slice(&chunk[0..20]);
        let node_id = NodeId::from_bytes(id_bytes);

        let addr = SocketAddr::V4(decode_compact_addr_v4(&chunk[20..26]));

        nodes.push(CompactNodeInfo { node_id, addr });
    }

    Ok(nodes)
}

/// Decode compact node info (supports both IPv4 and IPv6).
pub fn decode_compact_nodes(data: &[u8]) -> Result<Vec<CompactNodeInfo>, DhtError> {
    if data.len() % 26 != 0 && data.len() % 38 != 0 {
        return Err(DhtError::Parse(format!(
            "compact nodes length {} not divisible by 26 or 38",
            data.len()
        )));
    }

    let node_size = if data.len() % 38 == 0 { 38 } else { 26 };
    let mut nodes = Vec::with_capacity(data.len() / node_size);

    for chunk in data.chunks_exact(node_size) {
        let mut id_bytes = [0u8; 20];
        id_bytes.copy_from_slice(&chunk[0..20]);
        let node_id = NodeId::from_bytes(id_bytes);

        let addr = if node_size == 38 {
            decode_compact_addr_v6(&chunk[20..38])
                .ok_or_else(|| DhtError::Parse("Invalid IPv6 address".to_string()))?
        } else {
            SocketAddr::V4(decode_compact_addr_v4(&chunk[20..26]))
        };

        nodes.push(CompactNodeInfo { node_id, addr });
    }

    Ok(nodes)
}

/// Decode 6-byte compact address (4-byte IP + 2-byte port).
fn decode_compact_addr_v4(data: &[u8]) -> SocketAddrV4 {
    let ip = Ipv4Addr::new(data[0], data[1], data[2], data[3]);
    let port = u16::from_be_bytes([data[4], data[5]]);
    SocketAddrV4::new(ip, port)
}

/// Decode 18-byte compact IPv6 address (16-byte IP + 2-byte port).
fn decode_compact_addr_v6(data: &[u8]) -> Option<SocketAddr> {
    if data.len() != 18 {
        return None;
    }
    let ip = Ipv6Addr::from([
        data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8], data[9],
        data[10], data[11], data[12], data[13], data[14], data[15],
    ]);
    let port = u16::from_be_bytes([data[16], data[17]]);
    Some(SocketAddr::new(ip.into(), port))
}

// ============================================================================
// Compact encoding for peers (6 bytes each: 4-byte IP + 2-byte port)
// ============================================================================

/// Encode a single peer to compact format (6 bytes for IPv4, 18 bytes for IPv6).
pub fn encode_compact_peer(addr: &SocketAddr) -> Vec<u8> {
    let mut result = if addr.is_ipv6() {
        Vec::with_capacity(18)
    } else {
        Vec::with_capacity(6)
    };

    match addr {
        SocketAddr::V4(v4) => {
            result.extend_from_slice(&v4.ip().octets());
            result.extend_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            result.extend_from_slice(&v6.ip().octets());
            result.extend_from_slice(&v6.port().to_be_bytes());
        }
    }
    result
}

/// Encode a single peer to compact format (6 bytes).
#[allow(dead_code)]
fn encode_compact_peer_v4(addr: &SocketAddrV4) -> Vec<u8> {
    let mut result = Vec::with_capacity(6);
    result.extend_from_slice(&addr.ip().octets());
    result.extend_from_slice(&addr.port().to_be_bytes());
    result
}

/// Decode compact peer info (6 bytes per peer - IPv4 only).
pub fn decode_compact_peers_v4(data: &[u8]) -> Result<Vec<SocketAddr>, DhtError> {
    if data.len() % 6 != 0 {
        return Err(DhtError::Parse(format!(
            "compact peers length {} not divisible by 6",
            data.len()
        )));
    }

    let mut peers = Vec::with_capacity(data.len() / 6);
    for chunk in data.chunks_exact(6) {
        peers.push(SocketAddr::V4(decode_compact_addr_v4(chunk)));
    }

    Ok(peers)
}

// ============================================================================
// KRPC Error codes (BEP 0005)
// ============================================================================

pub mod error_codes {
    pub const GENERIC_ERROR: i64 = 201;
    pub const SERVER_ERROR: i64 = 202;
    pub const PROTOCOL_ERROR: i64 = 203;
    pub const METHOD_UNKNOWN: i64 = 204;
}
