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
//     let dht = DhtConfig::builder()
//         .state_dir("./data")
//         .build();
//     let dht = Dht::new(cfg).await.unwrap();
//
//     // Search for peers - just one call!
//     let peers = dht.announce(info_hash, 6881).await;
//     println!("Found {} peers", peers.len());
//
//     // Persist routing table and shut down.
//     dht.shutdown().await.unwrap();
// }
// ```
