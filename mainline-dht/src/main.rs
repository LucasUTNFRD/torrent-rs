//! Demo application for the BitTorrent Mainline DHT.
//!
//! This demonstrates:
//! 1. Creating a DHT node and bootstrapping into the network
//! 2. Retrieving peers for a specific infohash from the DHT
//!
//! Run with:
//! cargo run -- EA3849FFD066F77525A6DC41F2119DBD7130B540

use bittorrent_common::types::InfoHash;
use clap::Parser;
use mainline_dht::{Dht, DhtConfig};
use std::path::PathBuf;
use std::time::Instant;
use tracing::Level;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// info_hash to lookup peers for (hex string)
    infohash: String,

    /// Path to node ID file (persists identity across restarts)
    #[arg(short = 'i', long, default_value = None)]
    id_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let cli = Cli::parse();

    // To answer your question: Internally we use the 20-byte array (InfoHash).
    // But for the user interface, a hex string is much more convenient.
    let info_hash = InfoHash::from_hex(&cli.infohash)
        .expect("Invalid info_hash: must be a 40-character hex string");

    println!("Creating DHT node...");
    let config = if let Some(id_path) = cli.id_file {
        let state_path = id_path.parent().map(|p| p.join("dht_state.dat"));
        DhtConfig {
            id_file_path: Some(id_path),
            state_file_path: state_path,
            port: 6881,
        }
    } else {
        DhtConfig::with_default_persistence(6881)?
    };

    let dht = Dht::with_config(config).await?;

    println!("Bootstrapping into the DHT network...");
    dht.bootstrap().await?;
    println!("Bootstrap successful. Node ID: {}\n", dht.node_id());

    println!("Looking up peers for info_hash: {} ...", info_hash);

    println!("\n=== DHT QUERY ===");
    get_peers(&dht, info_hash).await?;

    // In a real application, you might want to run it again or keep the node running
    // to participate in the network.

    // Graceful shutdown
    dht.shutdown().await?;

    Ok(())
}

async fn get_peers(dht: &Dht, info_hash: InfoHash) -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();

    match dht.get_peers(info_hash).await {
        Ok(result) => {
            let elapsed = start.elapsed().as_millis();

            if result.peers.is_empty() {
                println!("Query finished in {} ms, but no peers were found.", elapsed);
                println!("Nodes contacted: {}", result.nodes_contacted);
            } else {
                println!("Got {} peers in {} ms:", result.peers.len(), elapsed);

                for (i, peer) in result.peers.iter().take(20).enumerate() {
                    println!("  {:2}. {}", i + 1, peer);
                }

                if result.peers.len() > 20 {
                    println!("  ... and {} more peers", result.peers.len() - 20);
                }
            }

            println!(
                "\nNodes with tokens available for announce: {}",
                result.nodes_with_tokens.len()
            );
        }
        Err(e) => {
            eprintln!("DHT lookup failed: {}", e);
        }
    }

    Ok(())
}
