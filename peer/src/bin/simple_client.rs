use std::sync::Arc;

use bittorrent_core::{metainfo::parse_torrent_from_file, types::PeerID};
use peer::{PeerInfo, PeerManagerHandle};
use tokio::net::TcpStream;
use tracker_client::TrackerHandler;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let torrent = parse_torrent_from_file("../sample_torrents/sample.torrent")
        .expect("failed to parse torrent");

    let torrent = Arc::new(torrent);

    let client_id = PeerID::new([0u8; 20]);

    let tracker = TrackerHandler::new(client_id);
    let peers = tracker
        .announce(torrent.clone())
        .await
        .expect("failed to announce to tracker")
        .peers;

    let manager = PeerManagerHandle::new();

    let mut handles = vec![];
    for addr in peers {
        tracing::info!("Connecting to {addr:?}");
        let info = PeerInfo::new(client_id, torrent.info_hash, addr);
        let manager = manager.clone();
        let handle = tokio::spawn(async move {
            if let Ok(stream) = TcpStream::connect(addr).await {
                match manager.add_peer(info, stream).await {
                    Ok(_) => tracing::info!("Connected to {addr:?}"),
                    Err(e) => tracing::error!("{e}"),
                }
            }
        });
        handles.push(handle);
    }
    for handle in handles {
        handle.await.expect("failed to join handle");
    }
}
