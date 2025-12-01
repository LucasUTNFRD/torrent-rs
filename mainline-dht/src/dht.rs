use std::{net::SocketAddr, time::Duration};

use bencode::{Bencode, BencodeDict};
use tokio::{net::UdpSocket, sync::mpsc};

use crate::{
    error::DhtError,
    message::{Message, Query},
    node_id::NodeId,
};

pub const DEFAULT_BOOTSTRAP_NODES: [&str; 4] = [
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "dht.libtorrent.org:25401",
    "relay.pkarr.org:6881",
];

// ---- SERVER ----
pub(crate) enum DhtCommand {}

pub struct Dht {
    pub(crate) sender: mpsc::Sender<DhtCommand>,
}

impl Dht {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(32);

        Self { sender: tx }
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

#[derive(Debug)]
pub(crate) struct DhtActor {
    command_rx: mpsc::Receiver<DhtCommand>,
    node_id: NodeId,
    routing_table: RoutingTable,
    socket: UdpSocket,
    transaction_id: u16,
}

#[derive(Debug)]
pub(crate) struct RoutingTable {}

enum NodeState {
    Good,
    Questionable,
    Bad,
}

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(50);
const DEFAULT_PORT: u16 = 6881;

impl DhtActor {
    pub async fn new(
        port: Option<u16>,
        command_rx: mpsc::Receiver<DhtCommand>,
    ) -> Result<Self, DhtError> {
        let bind_addr = if let Some(port) = port {
            format!("0.0.0.0:{port}")
        } else {
            format!("0.0.0.0:{DEFAULT_PORT}")
        };

        let (socket, node_id) =
            Self::bootstrap(&bind_addr, DEFAULT_BOOTSTRAP_NODES.to_vec()).await?;

        Ok(Self {
            socket,
            node_id,
            command_rx,
            routing_table: todo!(),
            transaction_id: 0,
        })
    }

    async fn bootstrap(
        bind_addr: &str,
        boostrap_node: Vec<&str>,
    ) -> Result<(UdpSocket, NodeId), DhtError> {
        let mut socket = UdpSocket::bind(bind_addr).await?;

        let mut node_id = NodeId::generate_random();

        let msg: Message = todo!();

        socket.send_to(&msg.to_bytes(), boostrap_node[0]).await?;

        let mut buffer = [0u8; 1024];
        let (size, addr) = socket
            .recv_from(&mut buffer)
            .await
            .expect("failed to read from udp socket");

        let response_bytes = &buffer[..size];

        // Decode the bencoded response
        let bencoded_response =
            Bencode::decode(response_bytes).expect("failed to decode bencode response");

        println!("\nReceived response from {} ({} bytes)", addr, size);

        let Bencode::Dict(bencode_dict) = bencoded_response else {
            panic!("invalid bencode response type");
        };

        let my_socket_addr = if let Some(socket_bytes) = bencode_dict.get_bytes(b"ip")
            && socket_bytes.len() == 6
        {
            let socket_bytes: [u8; 6] = socket_bytes.try_into().unwrap();
            let ip = std::net::Ipv4Addr::new(
                socket_bytes[0],
                socket_bytes[1],
                socket_bytes[2],
                socket_bytes[3],
            );
            let port = u16::from_be_bytes([socket_bytes[4], socket_bytes[5]]);
            SocketAddr::new(std::net::IpAddr::V4(ip), port)
        } else {
            panic!("ip was not received")
        };

        let replying_node_id = if let Some(response_dict) = bencode_dict.get_dict(b"r")
            && let Some(response_node_id) = response_dict.get_bytes(b"id")
        {
            let response_node_id: [u8; 20] = response_node_id.try_into().unwrap();
            response_node_id
        } else {
            panic!("node id was not recv")
        };

        println!("our_socket_addr:{my_socket_addr} ; replying_node_id:{replying_node_id:?}");

        // let mut secure_node_id_for_us = NodeId::generate_random();
        node_id.secure_node_id(&my_socket_addr.ip());

        Ok((socket, node_id))
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
