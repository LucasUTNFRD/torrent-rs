use mainline_dht::Dht;
use bittorrent_common::types::InfoHash;
use std::time::{Duration, Instant};

const TARGET_INFOHASH: &str = "AB9C23D3AE7A24F79CC8037900E0098376731282";
const ITERATIONS: usize = 2;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Only show errors/warnings to keep output clean
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();

    let info_hash = InfoHash::from_hex(TARGET_INFOHASH)
        .expect("Invalid info_hash");

    println!("Benchmarking DHT performance...");
    println!("Target InfoHash: {}", info_hash);
    println!("Iterations: {}", ITERATIONS);
    println!("--------------------------------------------------");

    let mut bootstrap_times = Vec::new();
    let mut get_peers_times = Vec::new();
    let mut peer_counts = Vec::new();

    for i in 1..=ITERATIONS {
        print!("Iteration {}: ", i);
        
        // Create new node with random port
        let dht = Dht::new(None).await?;
        
        // Measure Bootstrap
        let start_bootstrap = Instant::now();
        match dht.bootstrap().await {
            Ok(_) => {
                let duration = start_bootstrap.elapsed();
                bootstrap_times.push(duration);
                print!("Bootstrap: {:.2?} | ", duration);
            },
            Err(e) => {
                println!("Bootstrap failed: {}", e);
                continue;
            }
        }

        // Measure Get Peers
        let start_get_peers = Instant::now();
        match dht.get_peers(info_hash).await {
            Ok(result) => {
                let duration = start_get_peers.elapsed();
                get_peers_times.push(duration);
                peer_counts.push(result.peers.len());
                println!("GetPeers: {:.2?} ({} peers)", duration, result.peers.len());
            },
            Err(e) => {
                println!("GetPeers failed: {}", e);
            }
        }

        dht.shutdown().await?;
        
        // Small pause between iterations to release ports/resources
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    println!("--------------------------------------------------");
    println!("Results:");

    if !bootstrap_times.is_empty() {
        let avg_bootstrap: Duration = bootstrap_times.iter().sum::<Duration>() / bootstrap_times.len() as u32;
        let min_bootstrap = bootstrap_times.iter().min().unwrap();
        let max_bootstrap = bootstrap_times.iter().max().unwrap();
        println!("Bootstrap Time:");
        println!("  Avg: {:.2?}", avg_bootstrap);
        println!("  Min: {:.2?}", min_bootstrap);
        println!("  Max: {:.2?}", max_bootstrap);
    }

    if !get_peers_times.is_empty() {
        let avg_get_peers: Duration = get_peers_times.iter().sum::<Duration>() / get_peers_times.len() as u32;
        let min_get_peers = get_peers_times.iter().min().unwrap();
        let max_get_peers = get_peers_times.iter().max().unwrap();
        let avg_peers = peer_counts.iter().sum::<usize>() as f64 / peer_counts.len() as f64;
        
        println!("Get Peers Time:");
        println!("  Avg: {:.2?}", avg_get_peers);
        println!("  Min: {:.2?}", min_get_peers);
        println!("  Max: {:.2?}", max_get_peers);
        println!("  Avg Peers Found: {:.1}", avg_peers);
    }

    Ok(())
}
