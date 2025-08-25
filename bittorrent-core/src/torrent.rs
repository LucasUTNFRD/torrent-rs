use std::{sync::Arc, time::Duration};

use bittorrent_common::{metainfo::TorrentInfo, types::PeerID};
use peer::PeerManagerHandle;
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, UnboundedSender},
    task::JoinHandle,
    time::{Instant, interval_at},
};
use tracing::instrument;
use tracker_client::{TrackerError, TrackerHandler};

// Torrent Leeching abstraction
pub struct TorrentSession {
    handle: JoinHandle<Result<(), TorrentError>>,
    tx: UnboundedSender<TorrentMessage>,
}

enum TorrentMessage {
    Stats,
    Shutdown,
}

#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("Failed {0}")]
    Tracker(TrackerError),
}

impl TorrentSession {
    pub fn new(metainfo: TorrentInfo, tracker: Arc<TrackerHandler>, client_id: PeerID) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::task::spawn(async move {
            start_torrent_session(metainfo, tracker, client_id, rx).await
        });

        Self { handle, tx }
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(TorrentMessage::Shutdown);
    }
}

#[instrument(
    skip(tracker, manager_rx,client_id,metainfo),   
    fields(
        torrent.info_hash = %metainfo.info_hash,
    )
)]
async fn start_torrent_session(
    metainfo: TorrentInfo,
    tracker: Arc<TrackerHandler>,
    client_id: PeerID,
    mut manager_rx: mpsc::UnboundedReceiver<TorrentMessage>,
) -> Result<(), TorrentError> {
    let torrent = Arc::new(metainfo);

    let announce_resp = tracker
        .announce(torrent.clone())
        .await
        .map_err(TorrentError::Tracker)?;

    tracing::info!(?announce_resp);

    let mut announce_ticker = interval_at(
        Instant::now() + Duration::from_secs(announce_resp.interval as u64),
        Duration::from_secs(announce_resp.interval as u64),
    );

    let peer_manager = Arc::new(PeerManagerHandle::new(torrent.clone()));

    for peer_addr in announce_resp.peers {
        peer_manager.add_peer(peer_addr, client_id);
    }

    loop {
        tokio::select! {
            maybe_msg = manager_rx.recv() => {
                match maybe_msg{
                    Some(TorrentMessage::Shutdown) => {
                        peer_manager.shutdown();
                    }
                    Some(TorrentMessage::Stats) => {
                        todo!()
                    }
                    _ => break,
                }
            }
            _ = announce_ticker.tick() => {
                let tracker = tracker.clone();
                let torrent = torrent.clone();
                let peer_manager = peer_manager.clone();

                tokio::spawn(async move {
                    match tracker.announce(torrent).await {
                        Ok(resp) => {
                            tracing::info!(?resp);
                            for peer_addr in resp.peers {
                                peer_manager.add_peer(peer_addr, client_id);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(?e, "tracker announce failed");
                        }
                    }
                });
            }
        }
    }

    Ok(())
}
