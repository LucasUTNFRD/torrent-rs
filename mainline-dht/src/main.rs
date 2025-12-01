use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
};

use bencode::{Bencode, BencodeBuilder, BencodeDict};
use mainline_dht::node_id::NodeId;
use tokio::net::UdpSocket;

pub const DEFAULT_BOOTSTRAP_NODES: [&str; 4] = [
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "dht.libtorrent.org:25401",
    "relay.pkarr.org:6881",
];

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Bind a UDP socket to any available port
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("failed to bind socket");

    // Generate a random 20-byte node ID
    let node_id = NodeId::generate_random();
    let node_id_bytes = node_id.as_bytes();

    // ping Query = {"t":"aa", "y":"q", "q":"ping", "a":{"id":"abcdefghij0123456789"}}
    let mut args = BTreeMap::new();
    args.insert("id", node_id_bytes.as_slice());

    let mut query = BTreeMap::<Vec<u8>, Bencode>::new();
    query
        .put("t", &"aa")
        .put("y", &"q")
        .put("q", &"ping")
        .put("a", &args);

    let ping_query = query.build();
    let encoded = Bencode::encoder(&ping_query);

    println!("Sending ping query to {}", DEFAULT_BOOTSTRAP_NODES[0]);
    println!("Encoded message ({} bytes): {:?}", encoded.len(), encoded);

    // Send the ping to the first bootstrap node
    socket
        .send_to(&encoded, DEFAULT_BOOTSTRAP_NODES[0])
        .await
        .expect("failed to send ping");

    // Wait for response
    let mut buffer = [0u8; 1024];
    let (size, addr) = socket
        .recv_from(&mut buffer)
        .await
        .expect("failed to read from udp socket");

    let response_bytes = &buffer[..size];

    println!("\nReceived response from {} ({} bytes)", addr, size);

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

    let mut secure_node_id_for_us = NodeId::generate_random();
    secure_node_id_for_us.secure_node_id(&my_socket_addr.ip());
    println!(
        "our_socket_addr:{my_socket_addr} ; replying_node_id:{replying_node_id:?}; our_bep42_node_id:{secure_node_id_for_us:?}"
    );
}

// NOTE:
//  Dht::start()
// 1. Bind
// 2. Ping Bootstrap with Random ID
// 3. Get IP
// 4. Generate Secure ID
