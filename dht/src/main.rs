use anyhow::Context;
use bittorrent_common::types::InfoHash;
use dht::dht::{Dht, DhtConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up tracing with a simple level filter
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tracing::info!("Starting DHT visualization...");

    let cfg = DhtConfig::builder().state_dir(".").build();
    let dht = Dht::new(cfg).await.context("init dht")?;

    tracing::info!("DHT initialized, starting bootstrap...");

    let info_hash = InfoHash::from_hex("792B3577FED6DD95DBB03F5F0972E821230B834F").unwrap();

    // Wait for bootstrap to complete
    dht.wait_bootstrap()
        .await
        .context("failed to wait for bootstrap")?;
    tracing::info!("DHT bootstrap complete and stable!");

    let (v4_size, v6_size) = dht.get_routing_table_sizes().await.unwrap_or((0, 0));
    tracing::info!("Routing table size: v4={}, v6={}", v4_size, v6_size);

    tracing::info!("Finding peers for info hash: {}", info_hash.to_hex());
    let peers = dht
        .find_peers(info_hash)
        .await
        .context("failed to find peers")?;
    tracing::info!("Found {} peers", peers.len());
    for peer in peers {
        tracing::info!("Peer: {}", peer);
    }

    // Persist the routing table for faster startup next time.
    dht.shutdown().context("shutdown dht")?;

    Ok(())
}
