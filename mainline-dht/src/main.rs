//! Demo application for the BitTorrent Mainline DHT.
//!
//! This demonstrates:
//! 1. Creating a DHT node
//! 2. Bootstrapping into the network
//! 3. Finding nodes close to a target

use mainline_dht::{Dht, NodeId};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== BitTorrent Mainline DHT Demo ===\n");

    // Create a DHT node on a random port
    println!("Creating DHT node...");
    let dht = Dht::new(None).await?;
    println!("Initial node ID: {:?}\n", dht.node_id());

    // Bootstrap into the network
    println!("Bootstrapping into the DHT network...");
    match dht.bootstrap().await {
        Ok(node_id) => {
            println!("Bootstrap successful!");
            println!("BEP 42 secure node ID: {:?}", node_id);
        }
        Err(e) => {
            println!("Bootstrap failed: {e}");
            return Err(e.into());
        }
    }

    // Check routing table size
    let table_size = dht.routing_table_size().await?;
    println!("Routing table contains {} nodes\n", table_size);

    // Find nodes close to a random target
    let target = NodeId::generate_random();
    println!("Finding nodes close to target: {:?}", target);

    match dht.find_node(target).await {
        Ok(nodes) => {
            println!("Found {} nodes:", nodes.len());
            for (i, node) in nodes.iter().enumerate() {
                let distance = node.node_id ^ target;
                println!(
                    "  {}. {:?} @ {} (distance prefix: {:02x}{:02x})",
                    i + 1,
                    node.node_id,
                    node.addr,
                    distance.as_bytes()[0],
                    distance.as_bytes()[1]
                );
            }
        }
        Err(e) => {
            println!("find_node failed: {e}");
        }
    }

    println!("\n=== Demo Complete ===");

    // Shutdown
    dht.shutdown().await?;

    Ok(())
}
