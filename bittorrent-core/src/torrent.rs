use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize},
    },
    time::Duration,
};

use bittorrent_common::{
    metainfo::{Info, TorrentInfo},
    types::InfoHash,
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::{
        mpsc::{self, UnboundedSender},
        watch,
    },
    task::JoinHandle,
    time::sleep,
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

pub enum TorrentMessage {
    Shutdown,
    AddPeer(Peer),
}

// Move this to peer module?
pub enum Peer {
    Inbound {
        stream: TcpStream,
        remote_addr: SocketAddr,
        supports_ext: bool,
    },
    Outbound(SocketAddr), // Socket to connect + Our peer id
}

impl Peer {
    pub fn get_addr(&self) -> SocketAddr {
        match self {
            Self::Inbound {
                stream: _,
                remote_addr,
                supports_ext: _,
            } => *remote_addr,
            Self::Outbound(remote_addr) => *remote_addr,
        }
    }
}

#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("Failed {0}")]
    #[allow(dead_code)]
    Tracker(TrackerError),
}

impl TorrentSession {
    pub fn new(metainfo: TorrentInfo, tracker: Arc<TrackerHandler>, storage: Arc<Storage>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::task::spawn(async move {
            start_torrent_session(metainfo, tracker, storage, rx).await
        });

        Self { handle, tx }
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(TorrentMessage::Shutdown);
    }

    pub fn add_peer(&self, peer: Peer) {
        let _ = self.tx.send(TorrentMessage::AddPeer(peer));
    }
}

#[instrument(
    skip(tracker, manager_rx,metainfo,storage),
    fields(
        torrent.info_hash = %metainfo.info_hash,
        torrent.info.name = %metainfo.info.mode.name(),
    )
)]
async fn start_torrent_session(
    metainfo: TorrentInfo,
    tracker: Arc<TrackerHandler>,
    storage: Arc<Storage>,
    mut manager_rx: mpsc::UnboundedReceiver<TorrentMessage>,
) -> Result<(), TorrentError> {
    let torrent = Arc::new(metainfo);
    storage.add_torrent(torrent.clone());

    let info_hash = torrent.info_hash;
    let torrent_size = torrent.total_size();

    let (peer_manager, mut completion_rx) =
        PeerManagerHandle::new(torrent.clone(), storage.clone());

    let peer_manager = Arc::new(peer_manager);

    // TODO: Implement a announce_all
    let trackers = torrent.all_trackers();
    for t in trackers.into_iter() {
        let tracker = tracker.clone();
        let peer_manager = peer_manager.clone();
        let t = t.clone(); // clone each tracker entry if needed
        tokio::spawn(async move {
            run_announce_loop(tracker, info_hash, t, peer_manager, torrent_size).await
        });
    }

    loop {
        tokio::select! {
        maybe_msg = manager_rx.recv() => {
            match maybe_msg{
                Some(TorrentMessage::Shutdown) => {
                    peer_manager.shutdown();
                }
                Some(TorrentMessage::AddPeer(peer)) => {
                    peer_manager.add_peer(peer);
                }
                _ => break,
            }
        }
        _ = &mut completion_rx => {
            tracing::info!("finished downloading");
            break;
        }
        }
    }

    Ok(())
}

/// Loop in charge of announce and re-announcing to a single announce-url
async fn run_announce_loop(
    tracker_handler: Arc<TrackerHandler>,
    info_hash: InfoHash,
    announce_url: String,
    peer_manager: Arc<PeerManagerHandle>,
    torrent_size: i64,
) {
    let mut first_announce = true;
    let mut retry_count = 0;
    let mut interval_duration = Duration::from_secs(20); // Default retry interval
    const MAX_RETRIES: usize = 5;
    let mut bytes_left = torrent_size;

    loop {
        let stats = peer_manager.get_stats().await;

        // Implementer's Note: Even 30 peers is plenty, the official client version 3 in fact only actively forms new connections
        // if it has less than 30 peers and will refuse connections if it has 55.
        // This value is important to performance. When a new piece has completed download,
        // HAVE messages (see below) will need to be sent to most active peers.
        // As a result the cost of broadcast traffic grows in direct proportion to the number of peers.
        // Above 25, new peers are highly unlikely to increase download speed.
        // UI designers are strongly advised to make this obscure and hard to change as it is very rare to be useful to do so.
        if stats.connected_peers > 30 {
            sleep(interval_duration).await;
            continue;
        }

        bytes_left -= stats.downloaded as i64;

        let event = if first_announce {
            Events::Started
        } else {
            Events::None
        };
        let state = ClientState::new(
            stats.downloaded as i64,
            bytes_left,
            stats.uploaded as i64,
            event,
        );

        match tracker_handler
            .announce(info_hash, announce_url.clone(), state)
            .await
        {
            Ok(response) => {
                tracing::info!(
                    "recieve {} peers from : {announce_url}",
                    response.peers.len()
                );
                retry_count = 0;
                first_announce = false;
                for addr in response.peers {
                    peer_manager.add_peer(Peer::Outbound(addr));
                }
                interval_duration = Duration::from_secs(response.interval as u64);
            }
            Err(e) => {
                tracing::error!("Tracker announce error: {}", e);
                retry_count += 1;
                if retry_count >= MAX_RETRIES {
                    tracing::warn!("Max retries exceeded for tracker: {}", announce_url);
                    break;
                }
                // Exponential backoff with cap at 300 seconds
                interval_duration =
                    Duration::from_secs(20).mul_f32(2f32.powi(retry_count as i32 - 1));
                interval_duration = interval_duration.min(Duration::from_secs(300));
            }
        }

        sleep(interval_duration).await;
    }
}
