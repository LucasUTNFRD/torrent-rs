use std::{sync::Arc, time::Duration};

use bittorrent_common::{metainfo::TorrentInfo, types::PeerID};
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, UnboundedSender},
    task::JoinHandle,
    // time::{Instant, interval_at},
};
use tracing::instrument;
use tracker_client::{ClientState, Events, TrackerError, TrackerHandler};

use crate::{peer::manager::PeerManagerHandle, storage::Storage};

// Torrent Leeching abstraction
#[allow(dead_code)]
pub struct TorrentSession {
    handle: JoinHandle<Result<(), TorrentError>>,
    tx: UnboundedSender<TorrentMessage>,
}

enum TorrentMessage {
    // Stats,
    Shutdown,
}

#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("Failed {0}")]
    Tracker(TrackerError),
}

impl TorrentSession {
    pub fn new(
        metainfo: TorrentInfo,
        tracker: Arc<TrackerHandler>,
        client_id: PeerID,
        storage: Arc<Storage>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::task::spawn(async move {
            start_torrent_session(metainfo, tracker, client_id, storage, rx).await
        });

        Self { handle, tx }
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(TorrentMessage::Shutdown);
    }
}

#[instrument(
    skip(tracker, manager_rx,client_id,metainfo,storage),
    fields(
        torrent.info_hash = %metainfo.info_hash,
    )
)]
async fn start_torrent_session(
    metainfo: TorrentInfo,
    tracker: Arc<TrackerHandler>,
    client_id: PeerID,
    storage: Arc<Storage>,
    mut manager_rx: mpsc::UnboundedReceiver<TorrentMessage>,
) -> Result<(), TorrentError> {
    let torrent = Arc::new(metainfo);

    storage.add_torrent(torrent.clone());

    let trackers = torrent.all_trackers();
    let info_hash = torrent.info_hash;

    let (peer_manager, mut completion_rx) =
        PeerManagerHandle::new(torrent.clone(), storage.clone());

    let peer_manager = Arc::new(peer_manager);

    for t in trackers.into_iter() {
        let tracker = tracker.clone();
        let peer_manager = peer_manager.clone();
        let torrent = torrent.clone();
        let t = t.clone(); // clone each tracker entry if needed
        tokio::spawn(async move {
            let announce_resp = tracker
                .announce(
                    info_hash,
                    vec![t],
                    ClientState::new(0, torrent.total_size(), 0, Events::Started),
                )
                .await
                .map_err(TorrentError::Tracker);

            if let Err(e) = announce_resp {
                tracing::error!(?e);
                return;
            }

            let resp = announce_resp.unwrap();
            for addr in resp.peers {
                peer_manager.add_peer(addr, client_id);
            }
        });
    }

    loop {
        tokio::select! {
            maybe_msg = manager_rx.recv() => {
                match maybe_msg{
                    Some(TorrentMessage::Shutdown) => {
                        peer_manager.shutdown();
                    }
                    // Some(TorrentMessage::Stats) => {
                    //     todo!()
                    // }
                    _ => break,
                }
            }
             _ = &mut completion_rx => {
                tracing::info!("torrent {} finished downloading", torrent.info.mode.name());
                // TODO :Announce to tracker Event Completed

                // TODO we should run as seeder session

                break;
            }
        }
    }

    Ok(())
}
