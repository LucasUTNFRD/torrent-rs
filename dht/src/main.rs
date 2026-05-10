use anyhow::Context;
use bittorrent_common::types::InfoHash;
use dht::dht::{Dht, DhtConfig};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up tracing with a simple level filter
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tracing::info!("Starting DHT visualization...");

    let cfg = DhtConfig::default();
    let dht = Dht::new(cfg).await.context("failed to init dht")?;

    tracing::info!("DHT initialized, starting bootstrap...");

    let info_hash = InfoHash::from_hex("792B3577FED6DD95DBB03F5F0972E821230B834F").unwrap();

    // Periodically display routing table sizes
    let mut interval = tokio::time::interval(Duration::from_secs(2));

    loop {
        interval.tick().await;

        let status = dht
            .get_swarm_status()
            .await
            .unwrap_or(dht::dht::SwarmStatus::Broken);
        let (v4_size, v6_size) = dht.get_routing_table_sizes().await.unwrap_or((0, 0));

        tracing::info!(
            "--- DHT Status ---
            Swarm: {:?}
            IPv4 Nodes: {}
            IPv6 Nodes: {}
            ------------------",
            status,
            v4_size,
            v6_size
        );

        if matches!(status, dht::dht::SwarmStatus::Good) {
            tracing::info!("DHT bootstrap complete and stable!");
            break;
        }
    }

    tracing::info!("Finding peers for info hash: {}", info_hash.to_hex());
    let peers = dht
        .find_peers(info_hash)
        .await
        .context("failed to find peers")?;
    tracing::info!("Found {} peers", peers.len());
    for peer in peers {
        tracing::info!("Peer: {}", peer);
    }

    Ok(())
}
