use std::sync::Arc;

use bittorrent_core::{metainfo::parse_torrent_from_file, types::PeerID};
use peer::PeerManagerHandle;
use tracker_client::TrackerHandler;

// TODO: Flaws of this:
// use need to take address from peer and need to perform tcp connections, and then also await on
// this
// what we actually want is to await on peer manager ending
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let torrent = parse_torrent_from_file("../sample_torrents/sample.torrent")
        .expect("failed to parse torrent");

    let torrent = Arc::new(torrent);

    let client_id = PeerID::new([0u8; 20]);

    let tracker = TrackerHandler::new(client_id);
    let tracker_resp = tracker
        .announce(torrent.clone())
        .await
        .expect("failed to announce to tracker");

    let manager = PeerManagerHandle::new(torrent.clone());
    for addr in tracker_resp.peers {
        manager
            .add_peer(addr,client_id)
            .unwrap();
    }
    // Park until Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl_c");
}
