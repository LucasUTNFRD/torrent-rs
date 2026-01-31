use std::collections::BTreeMap;

use bencode::{Bencode, BencodeBuilder};

use crate::{error::DhtError, node_id::NodeId};

pub struct Message {
    pub(crate) transaction_id: TransactionId,
    pub(crate) msg_type: MessageType,
    pub(crate) client_version: Option<i32>,
}

impl Message {
    pub fn from_bytes(bytes: &[u8]) -> Result<Message, DhtError> {
        todo!()
    }

    // A KRPC message is a single dictionary with three keys common to every message and additional keys depending on the type of message.
    // Every message has:
    // 1. a key "t" with a string value representing a transaction ID.
    // 2.  Every message also has a key "y" with a single character value describing the type of message
    // 3. A key "v" should be included in every message with a client version string. The string should be a two character client identifier registered in BEP 20  followed by a two character version identifier.
    pub fn to_bytes(&self) -> Vec<u8> {
        // Generate a random 20-byte node ID
        let node_id = NodeId::generate_random();
        let node_id_bytes = node_id.as_bytes();

        let transaction_id = self.transaction_id;
        let message_type = self.msg_type.as_str();

        // ping Query = {"t":"aa", "y":"q", "q":"ping", "a":{"id":"abcdefghij0123456789"}}
        let mut args = BTreeMap::new();
        args.insert("id", node_id_bytes.as_slice());

        let mut query = BTreeMap::<Vec<u8>, Bencode>::new();
        query
            .put("t", &"aa")
            .put("y", &"q") //
            .put("q", &"ping") // msg_type
            .put("a", &args);

        let ping_query = query.build();

        Bencode::encoder(&ping_query)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TransactionId(u16);

pub enum MessageType {
    Error,
    Query(Query),
    Response(Response),
}

impl MessageType {
    pub fn as_str(&self) -> &str {
        match self {
            MessageType::Error => "e",
            MessageType::Query(_) => "q",
            MessageType::Response(_) => "r",
        }
    }
}

pub enum Error {
    Generic,
    Server,
    Protocol,
    Unknown,
}

///For the DHT protocol, there are four queries: ping, find_node, get_peers, and announce_peer.
pub enum Query {
    Ping(PingRequest),
    FindNode,
    GetPeers,
    AnnoucePeer,
}

pub enum Response {
    Ping(PingResponse),
}

#[derive(Debug)]
struct PingRequest {}

#[derive(Debug)]
struct PingResponse {}
