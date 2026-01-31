//! KRPC Protocol message types for BitTorrent DHT (BEP 0005).
//!
//! The KRPC protocol uses bencoded dictionaries over UDP with three message types:
//! - Query (y = "q"): Request from one node to another
//! - Response (y = "r"): Successful reply to a query  
//! - Error (y = "e"): Error reply to a query

use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddrV4},
};

use bencode::{Bencode, BencodeBuilder, BencodeDict};

use crate::{error::DhtError, node_id::NodeId};

/// Transaction ID for correlating requests and responses.
/// Typically 2 bytes, sufficient for 65536 outstanding queries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransactionId(pub Vec<u8>);

impl TransactionId {
    pub fn new(id: u16) -> Self {
        Self(id.to_be_bytes().to_vec())
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
    pub version: Option<Vec<u8>>,
    /// Sender's external IP as seen by responder (BEP 42).
    pub sender_ip: Option<SocketAddrV4>,
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

/// DHT query types (ping and find_node for minimal implementation).
#[derive(Debug)]
pub enum Query {
    Ping { id: NodeId },
    FindNode { id: NodeId, target: NodeId },
}

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
}

/// Compact node info: 20-byte node ID + 6-byte IP:port.
#[derive(Debug, Clone)]
pub struct CompactNodeInfo {
    pub node_id: NodeId,
    pub addr: SocketAddrV4,
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
                        args.put("id", &id.as_bytes().as_slice());
                        dict.insert(b"a".to_vec(), args.build());
                    }
                    Query::FindNode { id, target } => {
                        dict.put("q", &"find_node");
                        let mut args = BTreeMap::<Vec<u8>, Bencode>::new();
                        args.put("id", &id.as_bytes().as_slice());
                        args.put("target", &target.as_bytes().as_slice());
                        dict.insert(b"a".to_vec(), args.build());
                    }
                }
            }
            MessageBody::Response(response) => {
                dict.put("y", &"r");
                let mut r = BTreeMap::<Vec<u8>, Bencode>::new();
                match response {
                    Response::Ping { id } => {
                        r.put("id", &id.as_bytes().as_slice());
                    }
                    Response::FindNode { id, nodes } => {
                        r.put("id", &id.as_bytes().as_slice());
                        let compact = encode_compact_nodes_v4(nodes);
                        r.put("nodes", &compact.as_slice());
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
        let transaction_id = TransactionId(transaction_id.to_vec());

        // Version (optional)
        let version = dict.get_bytes(b"v").map(|v| v.to_vec());

        // Sender IP (optional, BEP 42)
        let sender_ip = dict.get_bytes(b"ip").and_then(|ip_bytes| {
            if ip_bytes.len() == 6 {
                Some(decode_compact_addr_v4(ip_bytes))
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
    pub fn find_node_query(tx_id: u16, node_id: NodeId, target: NodeId) -> Self {
        Self {
            transaction_id: TransactionId::new(tx_id),
            version: None,
            sender_ip: None,
            body: MessageBody::Query(Query::FindNode {
                id: node_id,
                target,
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

    /// Get the node ID from this message (if it's a response).
    pub fn get_node_id(&self) -> Option<NodeId> {
        match &self.body {
            MessageBody::Response(Response::Ping { id }) => Some(*id),
            MessageBody::Response(Response::FindNode { id, .. }) => Some(*id),
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

    match method {
        "ping" => Ok(MessageBody::Query(Query::Ping { id })),
        "find_node" => {
            let target = parse_node_id(args, b"target")?;
            Ok(MessageBody::Query(Query::FindNode { id, target }))
        }
        _ => Err(DhtError::Parse(format!("unknown query method: {method}"))),
    }
}

fn parse_response(dict: &BTreeMap<Vec<u8>, Bencode>) -> Result<MessageBody, DhtError> {
    let r = dict
        .get_dict(b"r")
        .ok_or_else(|| DhtError::Parse("missing response body".to_string()))?;

    let id = parse_node_id(r, b"id")?;

    // Check if this is a find_node response (has "nodes" key)
    if let Some(nodes_bytes) = r.get_bytes(b"nodes") {
        let nodes = decode_compact_nodes_v4(nodes_bytes)?;
        Ok(MessageBody::Response(Response::FindNode { id, nodes }))
    } else {
        // Assume ping response if no "nodes" key
        Ok(MessageBody::Response(Response::Ping { id }))
    }
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
// Compact encoding for nodes (26 bytes each: 20-byte ID + 4-byte IP + 2-byte port)
// ============================================================================

/// Encode a list of nodes to compact format (26 bytes per node).
pub fn encode_compact_nodes_v4(nodes: &[CompactNodeInfo]) -> Vec<u8> {
    let mut result = Vec::with_capacity(nodes.len() * 26);
    for node in nodes {
        result.extend_from_slice(&node.node_id.as_bytes());
        result.extend_from_slice(&node.addr.ip().octets());
        result.extend_from_slice(&node.addr.port().to_be_bytes());
    }
    result
}

/// Decode compact node info (26 bytes per node).
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

        let addr = decode_compact_addr_v4(&chunk[20..26]);

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

// ============================================================================
// KRPC Error codes (BEP 0005)
// ============================================================================

pub mod error_codes {
    pub const GENERIC_ERROR: i64 = 201;
    pub const SERVER_ERROR: i64 = 202;
    pub const PROTOCOL_ERROR: i64 = 203;
    pub const METHOD_UNKNOWN: i64 = 204;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_ping_query() {
        let node_id = NodeId::generate_random();
        let msg = KrpcMessage::ping_query(42, node_id);

        let bytes = msg.to_bytes();
        let decoded = KrpcMessage::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.transaction_id.0, vec![0, 42]);
        match decoded.body {
            MessageBody::Query(Query::Ping { id }) => {
                assert_eq!(id, node_id);
            }
            _ => panic!("expected ping query"),
        }
    }

    #[test]
    fn encode_decode_find_node_query() {
        let node_id = NodeId::generate_random();
        let target = NodeId::generate_random();
        let msg = KrpcMessage::find_node_query(123, node_id, target);

        let bytes = msg.to_bytes();
        let decoded = KrpcMessage::from_bytes(&bytes).unwrap();

        match decoded.body {
            MessageBody::Query(Query::FindNode { id, target: t }) => {
                assert_eq!(id, node_id);
                assert_eq!(t, target);
            }
            _ => panic!("expected find_node query"),
        }
    }

    #[test]
    fn encode_decode_find_node_response() {
        let node_id = NodeId::generate_random();
        let nodes = vec![
            CompactNodeInfo {
                node_id: NodeId::generate_random(),
                addr: SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 6881),
            },
            CompactNodeInfo {
                node_id: NodeId::generate_random(),
                addr: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 8080),
            },
        ];

        let tx_id = TransactionId::new(999);
        let msg = KrpcMessage::find_node_response(tx_id, node_id, nodes.clone());

        let bytes = msg.to_bytes();
        let decoded = KrpcMessage::from_bytes(&bytes).unwrap();

        match decoded.body {
            MessageBody::Response(Response::FindNode {
                id,
                nodes: decoded_nodes,
            }) => {
                assert_eq!(id, node_id);
                assert_eq!(decoded_nodes.len(), 2);
                assert_eq!(decoded_nodes[0].node_id, nodes[0].node_id);
                assert_eq!(decoded_nodes[0].addr, nodes[0].addr);
            }
            _ => panic!("expected find_node response"),
        }
    }

    #[test]
    fn compact_node_encoding() {
        let nodes = vec![CompactNodeInfo {
            node_id: NodeId::from_bytes([1u8; 20]),
            addr: SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 6881),
        }];

        let encoded = encode_compact_nodes_v4(&nodes);
        assert_eq!(encoded.len(), 26);

        let decoded = decode_compact_nodes_v4(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].node_id, nodes[0].node_id);
        assert_eq!(decoded[0].addr, nodes[0].addr);
    }
}
