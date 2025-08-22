use std::sync::Arc;

use bittorrent_common::{metainfo::TorrentInfo, types::PeerID};
use peer::PeerManagerHandle;
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, UnboundedSender},
    task::JoinHandle,
    time::{Instant, interval_at},
};
use tracker_client::{TrackerError, TrackerHandler};

// Torrent Leeching abstraction
pub struct TorrentSession {
    handle: JoinHandle<Result<(), TorrentError>>,
    tx: UnboundedSender<TorrentMessage>,
}

enum TorrentMessage {}

#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("Failed {0}")]
    Tracker(TrackerError),
}

impl TorrentSession {
    pub fn new(metainfo: TorrentInfo, tracker: Arc<TrackerHandler>, client_id: PeerID) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::task::spawn(async move {
            let torrent = Arc::new(metainfo);

            let announce_resp = tracker
                .announce(torrent.clone())
                .await
                .map_err(TorrentError::Tracker)?;

            tracing::info!(?announce_resp);

            let peer_manager = PeerManagerHandle::new(torrent.clone());

            for peer_addr in announce_resp.peers {
                peer_manager.add_peer(peer_addr, client_id);
            }

            let _ = peer_manager.handle.await;

            Ok(())
        });
        Self { handle, tx }
    }
}
