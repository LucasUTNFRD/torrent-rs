use std::{net::SocketAddr, sync::Arc, time::Duration};

use bittorrent_common::{
    metainfo::TorrentInfo,
    types::{InfoHash, PeerID},
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{self, UnboundedSender},
    task::JoinHandle,
    time::{Instant, interval, sleep}, // time::{Instant, interval_at},
};
use tracing::instrument;
use tracker_client::{ClientState, Events, TrackerError, TrackerHandler};

use crate::{peer::manager::PeerManagerHandle, session::CLIENT_ID, storage::Storage};

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

pub struct TorrentStats {
    /// When the torrent was _first_ started.
    pub start_time: Instant,

    /// How long the torrent has been running.
    pub run_duration: Duration,

    /// Aggregate statistics about a torrent's pieces.
    pub remaining_pieces: usize,

    /// The peers of the torrent.
    pub connected_peers: usize,

    pub downloaded: u64,
    pub uploaded: u64,

    pub session_duration: Duration,
    pub progress: f64,

    pub download_rate: f64,
    pub upload_rate: f64,
}

pub struct StatsTracker {
    last_downloaded: u64,
    last_uploaded: u64,
    last_instant: Instant,
}

impl StatsTracker {
    pub fn new() -> Self {
        Self {
            last_downloaded: 0,
            last_uploaded: 0,
            last_instant: Instant::now(),
        }
    }

    pub fn update(&mut self, mut stats: TorrentStats) -> TorrentStats {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_instant).as_secs_f64();

        let dl_delta = stats.downloaded.saturating_sub(self.last_downloaded);
        let ul_delta = stats.uploaded.saturating_sub(self.last_uploaded);

        stats.download_rate = dl_delta as f64 / elapsed;
        stats.upload_rate = ul_delta as f64 / elapsed;

        self.last_downloaded = stats.downloaded;
        self.last_uploaded = stats.uploaded;
        self.last_instant = now;

        stats
    }
}

impl TorrentSession {
    pub fn new(
        metainfo: TorrentInfo,
        tracker: Arc<TrackerHandler>,
        storage: Arc<Storage>,
        stats_tx: mpsc::UnboundedSender<TorrentStats>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::task::spawn(async move {
            start_torrent_session(metainfo, tracker, storage, rx, stats_tx).await
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
    skip(tracker, manager_rx,metainfo,storage,stats_tx),
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
    stats_tx: mpsc::UnboundedSender<TorrentStats>,
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

    let mut stat_sender_interval = interval(Duration::from_secs(1));
    let total_pieces = torrent.num_pieces();

    let start_time = Instant::now();

    let mut session_duration = Duration::default();
    let mut stats_tracker = StatsTracker::new();

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
            _ = stat_sender_interval.tick() => {
                let peer_stats = peer_manager.get_stats().await;
                session_duration = Instant::now() - start_time;

                let progress = 100.0 * (total_pieces.saturating_sub(peer_stats.remaining_pieces) as f64)
                    / (total_pieces as f64);

                let raw_stats = TorrentStats {
                    start_time,
                    run_duration: session_duration,
                    remaining_pieces: peer_stats.remaining_pieces,
                    connected_peers: peer_stats.connected_peers,
                    downloaded: peer_stats.downloaded,
                    uploaded: peer_stats.uploaded,
                    session_duration,
                    progress,
                    download_rate: 0.0,
                    upload_rate: 0.0,
                };

                let stats = stats_tracker.update(raw_stats);
                let _ = stats_tx.send(stats);
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
