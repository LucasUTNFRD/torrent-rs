pub mod dht;
mod error;
mod message;
mod node_id;
mod peer_store;
mod routing_table;
mod token;

// ```rust
// /// This is what users see - super simple!
// #[tokio::main]
// async fn main() {
//     // Build DHT with one line
//     let dht = DhtConfig::new()
//         .peer_port(6881)
//         .with_default_bootstraps()
//         .state_file("./dht.dat")
//         .build();
//
//     // Search for peers - just one call!
//     let peers = dht.search(info_hash, 6881).await;
//     println!("Found {} peers", peers.len());
//
//     // Or don't announce, just search
//     let peers = dht.find_peers(info_hash).await;
// }
// ```
