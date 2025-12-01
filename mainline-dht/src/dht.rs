use std::net::SocketAddr;

use tokio::sync::mpsc;

use crate::{error::DhtError, message::Message, node_id::NodeId};

const BOOTSTRAP_NODES: [&str; 1] = ["router.bittorrent.com:6881"];

// ---- SERVER ----
pub(crate) enum DhtCommand {}

pub struct Dht {
    pub(crate) sender: mpsc::Sender<DhtCommand>,
}

impl Dht {
    pub fn new() -> Self {
        todo!()
    }

    // Get peers associated with a torrent infohash.
    // "q" = "get_peers" A get_peers query has two arguments, "id" containing the node ID of the querying node, and "info_hash" containing the infohash of the torrent.
    // If the queried node has peers for the infohash, they are returned in a key "values" as a list of strings.
    // Each string containing "compact" format peer information for a single peer.
    // If the queried node has no peers for the infohash, a key "nodes" is returned containing the K nodes in the queried nodes routing table closest to the infohash supplied in the query.
    // In either case a "token" key is also included in the return value.
    // The token value is a required argument for a future announce_peer query.
    // The token value should be a short binary string.
    pub async fn get_peers() {}

    // Announce that the peer, controlling the querying node, is downloading a torrent on a port.
    // announce_peer has four arguments: "id" containing the node ID of the querying node, "info_hash" containing the infohash of the torrent, "port" containing the port as an integer, and the "token" received in response to a previous get_peers query.
    // The queried node must verify that the token was previously sent to the same IP address as the querying node.
    // Then the queried node should store the IP address of the querying node and the supplied port number under the infohash in its store of peer contact information.
    pub async fn announce_peer() {}
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DhtActor {
    node_id: NodeId,
    listen_addr: SocketAddr,
}

pub struct Node {}

impl DhtActor {
    pub fn new(listen_addr: SocketAddr) -> Self {
        let mut node_id = NodeId::generate_random();
        node_id.secure_node_id(&listen_addr.ip());

        Self {
            node_id,
            listen_addr,
        }
    }

    pub(crate) fn ping(&mut self) {}

    pub(crate) fn get_peer(&mut self) {}

    fn send(&mut self, msg: Message) -> Result<(), DhtError> {
        todo!()
    }
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;

    use bencode::{Bencode, BencodeBuilder};

    #[test]
    fn serialize_ping_queries_example_packets() {
        // ping Query = {"t":"aa", "y":"q", "q":"ping", "a":{"id":"abcdefghij0123456789"}}
        // bencoded = d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe

        let mut args = BTreeMap::new();
        args.insert("id", "abcdefghij0123456789");

        let mut query = BTreeMap::<Vec<u8>, Bencode>::new();
        query
            .put("t", &"aa")
            .put("y", &"q")
            .put("q", &"ping")
            .put("a", &args); // args implements Encode, so this works!

        let ping_query = query.build();
        let encoded = Bencode::encoder(&ping_query);
        let expected = b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe";

        assert_eq!(encoded, expected);

        // Response = {"t":"aa", "y":"r", "r": {"id":"mnopqrstuvwxyz123456"}}
        // bencoded = d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:aa1:y1:re

        let mut response_data = BTreeMap::new();
        response_data.insert("id", "mnopqrstuvwxyz123456");

        let mut response = BTreeMap::<Vec<u8>, Bencode>::new();
        response
            .put("t", &"aa")
            .put("y", &"r")
            .put("r", &response_data);

        let ping_response = response.build();
        let encoded_response = Bencode::encoder(&ping_response);
        let expected_response = b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:aa1:y1:re";

        assert_eq!(encoded_response, expected_response);

        let decoded = Bencode::decode(&encoded).unwrap();
        let re_encoded = Bencode::encoder(&decoded);
        assert_eq!(re_encoded, expected);
    }
}
