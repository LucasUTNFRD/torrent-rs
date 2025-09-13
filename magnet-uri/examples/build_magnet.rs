use bittorrent_common::types::InfoHash;
use magnet_uri::{Magnet, PeerAddr};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use url::Url;

fn main() {
    // Create an infohash from a hex string
    let infohash =
        InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").expect("Invalid infohash");

    // Create tracker URLs
    let tracker1 = Url::parse("udp://tracker1.example.com:6969").unwrap();
    let tracker2 = Url::parse("udp://tracker2.example.com:6969").unwrap();

    // Create peer addresses - both IP-based and hostname-based
    let peer1 = PeerAddr::Socket(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        6881,
    ));

    let peer2 = PeerAddr::HostPort {
        host: "peer.example.com".to_string(),
        port: 1337,
    };

    // Build the magnet URI
    let magnet = Magnet::new(infohash)
        .with_display_name("Example Torrent")
        .add_tracker(tracker1)
        .add_tracker(tracker2)
        .add_peer(peer1)
        .add_peer(peer2);

    // Display the created magnet object
    println!("Created magnet URI with the following properties:");
    println!(
        "Display name: {}",
        magnet.display_name.as_ref().unwrap_or(&String::new())
    );

    if let Some(infohash) = magnet.info_hash() {
        println!("InfoHash: {}", infohash);
    }

    println!("\nTrackers ({}):", magnet.trackers.len());
    for (i, tracker) in magnet.trackers.iter().enumerate() {
        println!("  {}: {}", i + 1, tracker);
    }

    println!("\nPeers ({}):", magnet.peers.len());
    for (i, peer) in magnet.peers.iter().enumerate() {
        println!("  {}: {}", i + 1, peer);
    }

    // Convert the magnet struct to a URI string using Display implementation
    println!("\nMagnet URI:");
    println!("{}", magnet);
}
