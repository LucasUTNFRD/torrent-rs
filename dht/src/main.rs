use anyhow::Context;
use bittorrent_common::types::InfoHash;
use dht::dht::{Dht, DhtConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = DhtConfig::default();
    let dht = Dht::new(cfg).await.context("failed to init dht")?;

    let info_hash = InfoHash::from_hex("76ED6A73E91B8A14035CA3B1F05E9F885598F0BB")
        .context("failed to parse infohash")?;

    let found_peers = dht
        .find_peers(info_hash)
        .await
        .context("failed to fetch peers")?;

    Ok(())
}
