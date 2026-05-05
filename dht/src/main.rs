use anyhow::Context;
use bittorrent_common::types::InfoHash;
use dht::dht::{Dht, DhtConfig};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = DhtConfig::default();
    let dht = Dht::new(cfg).await.context("failed to init dht")?;

    // let info_hash = InfoHash::from_hex("76ED6A73E91B8A14035CA3B1F05E9F885598F0BB")
    //     .context("failed to parse infohash")?;

    // sleep(Duration::from_secs(60)).await;
    // let found_peers = dht
    //     .find_peers(info_hash)
    //     .await
    //     .context("failed to fetch peers")?;

    loop {
        let status = dht.get_swarm_status().await.unwrap();
        tracing::info!("Swarm status: {:?}", status);
        match status {
            dht::dht::SwarmStatus::Good => {
                tracing::info!("DHT bootstrap complete - ready to find peers");
                break;
            }
            _ => {
                sleep(Duration::from_secs(2)).await;
            }
        }
    }

    // Now ready to use DHT for peer discovery
    // let found_peers = dht.find_peers(info_hash).await...;

    Ok(())
}
