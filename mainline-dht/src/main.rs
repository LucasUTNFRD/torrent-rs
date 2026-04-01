//! Demo application for the BitTorrent Mainline DHT.
//!
//! This demonstrates:
//! 1. Creating a DHT node with empty routing table (no persistence)
//! 2. Bootstrapping into the network with timeout/success tracking
//! 3. Retrieving peers for a specific infohash from the DHT
//!
//! Run with:
//! cargo run -- EA3849FFD066F77525A6DC41F2119DBD7130B540

use bittorrent_common::types::InfoHash;
use clap::Parser;
use mainline_dht::{Dht, DhtConfig};
use std::time::Instant;
use tracing::Level;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// info_hash to lookup peers for (hex string)
    infohash: String,
    /// Port to bind DHT to
    #[arg(short, long, default_value = "6881")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let cli = Cli::parse();

    let info_hash = InfoHash::from_hex(&cli.infohash)
        .expect("Invalid info_hash: must be a 40-character hex string");

    println!("=== DHT Bootstrap Test ===");
    println!("Infohash: {}", info_hash);
    println!("Port: {}", cli.port);
    println!();

    // Create DHT with NO persistence - starts with empty routing table
    println!("Creating DHT node (empty routing table)...");
    let config = DhtConfig {
        id_file_path: None,    // No persisted node ID
        state_file_path: None, // No persisted routing table
        port: cli.port,
    };

    let start = Instant::now();
    let dht = Dht::with_config(config).await?;
    let bootstrap_time = start.elapsed();

    println!("Node ID: {}", dht.node_id());
    println!("Bootstrap time: {:?}", bootstrap_time);
    println!(
        "Routing table size: {} nodes",
        dht.routing_table_size().await?
    );
    println!();

    if dht.routing_table_size().await? == 0 {
        println!("WARNING: Bootstrap failed - no nodes in routing table!");
        println!("This could be due to:");
        println!("  - Network connectivity issues");
        println!("  - Bootstrap nodes unreachable");
        println!("  - Firewall blocking UDP port {}", cli.port);
    }

    println!("=== Starting get_peers lookup ===");
    println!("Looking up peers for: {}", info_hash);

    let start = Instant::now();
    let peer_count = get_peers(&dht, info_hash).await?;
    let lookup_time = start.elapsed();

    println!();
    println!("=== Results ===");
    println!("Peers found: {}", peer_count);
    println!("Lookup time: {:?}", lookup_time);
    println!(
        "Final routing table size: {} nodes",
        dht.routing_table_size().await?
    );

    // Graceful shutdown
    dht.shutdown().await?;

    Ok(())
}

async fn get_peers(dht: &Dht, info_hash: InfoHash) -> Result<usize, Box<dyn std::error::Error>> {
    let mut peer_recv = dht.get_peers(info_hash).await;
    let mut total_peers = 0;
    let mut batches = 0;

    println!("Waiting for peers...");
    while let Some(peers) = peer_recv.recv().await {
        batches += 1;
        total_peers += peers.len();
        println!("  Batch {}: {} peers", batches, peers.len());
        for peer in &peers {
            println!("    - {}", peer);
        }

        // Stop after receiving some peers or timeout
        if total_peers >= 50 {
            println!("Received enough peers, stopping...");
            break;
        }
    }

    if total_peers == 0 {
        println!("No peers found for this infohash");
    }

    Ok(total_peers)
}
