use mainline_dht::Dht;
use bittorrent_common::types::InfoHash;
use clap::Parser;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Number of benchmark iterations
    #[arg(short, long, default_value_t = 3)]
    iterations: usize,

    /// Infohash to look up (hex string)
    #[arg(long, default_value = "AB9C23D3AE7A24F79CC8037900E0098376731282")]
    infohash: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Only show errors/warnings to keep output clean
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();

    let args = Args::parse();
    let info_hash = InfoHash::from_hex(&args.infohash)
        .expect("Invalid info_hash");

    println!("==================================================");
    println!("DHT Performance Benchmark");
    println!("==================================================");
    println!("Target:     {}", info_hash);
    println!("Iterations: {}", args.iterations);
    println!("--------------------------------------------------");

    let mut bootstrap_times = Vec::new();
    let mut get_peers_times = Vec::new();
    let mut peer_counts = Vec::new();

    for i in 1..=args.iterations {
        // Create new node with random port
        let dht = Dht::new(None).await?;
        
        // Measure Bootstrap
        print!("Iter {:<2} | Bootstrap... ", i);
        let start_bootstrap = Instant::now();
        match dht.bootstrap().await {
            Ok(_) => {
                let duration = start_bootstrap.elapsed();
                bootstrap_times.push(duration);
                print!("{:<8.2?} | ", duration);
            },
            Err(e) => {
                println!("Failed: {}", e);
                continue;
            }
        }

        // Measure Get Peers
        print!("GetPeers... ");
        let start_get_peers = Instant::now();
        match dht.get_peers(info_hash).await {
            Ok(result) => {
                let duration = start_get_peers.elapsed();
                get_peers_times.push(duration);
                peer_counts.push(result.peers.len());
                println!("{:<8.2?} ({:<3} peers)", duration, result.peers.len());
            },
            Err(e) => {
                println!("Failed: {}", e);
            }
        }

        dht.shutdown().await?;
        
        // Small pause between iterations
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    println!("==================================================");
    
    if bootstrap_times.is_empty() {
        println!("No successful iterations completed.");
        return Ok(());
    }

    let avg_bootstrap: Duration = bootstrap_times.iter().sum::<Duration>() / bootstrap_times.len() as u32;
    let avg_get_peers: Duration = get_peers_times.iter().sum::<Duration>() / get_peers_times.len() as u32;
    let avg_peers = peer_counts.iter().sum::<usize>() as f64 / peer_counts.len() as f64;

    println!("RESULTS (Average of {} runs)", args.iterations);
    println!("--------------------------------------------------");
    println!("Bootstrap Time : {:.2?}", avg_bootstrap);
    println!("GetPeers Time  : {:.2?}", avg_get_peers);
    println!("Avg Peers Found: {:.1}", avg_peers);
    println!("==================================================");

    Ok(())
}
