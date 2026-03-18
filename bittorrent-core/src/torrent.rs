use crate::{
    StorageBackend, TorrentProgress, TorrentState,
    bitfield::Bitfield,
    choker::Choker,
    ema::EmaRate,
    events::{EventBus, SessionEvent},
    metadata::{Metadata, MetadataState},
    metrics::counters,
    net::TcpStream,
    peer::{
        PeerMessage,
        peer_connection::{ConnectionError, PeerHandle, spawn_inbound, spawn_outbound},
    },
    piece_picker::{AvailabilityUpdate, BlockRequest, PieceManager, PieceState},
    protocol::peer_wire::{Block, BlockInfo, Message},
};
use bittorrent_common::{
    metainfo::{Info, TorrentInfo},
    types::{InfoHash, PeerID},
};
use bytes::Bytes;
use magnet_uri::Magnet;
use mainline_dht::DhtHandler;
// use peer_protocol::protocol::{Block, BlockInfo, Message};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::sleep,
};
use tracing::{debug, info, instrument, warn};
use tracker_client::{ClientState, Events, TrackerError, TrackerHandler};
use url::Url;

// TODO: Use a criteria for this, this is so harcoded lol
const CHANNEL_SIZE: usize = 1000;

// Peer related
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Pid(pub usize);

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

static PEER_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub enum PeerOrigin {
    Inbound {
        stream: TcpStream,
        remote_addr: SocketAddr,
        supports_ext: bool,
        peer_id: PeerID,
    },
    Outbound(SocketAddr),
}

impl PeerOrigin {
    pub const fn get_addr(&self) -> &SocketAddr {
        match self {
            Self::Inbound {
                stream: _,
                remote_addr,
                supports_ext: _,
                peer_id: _,
            }
            | Self::Outbound(remote_addr) => remote_addr,
        }
    }
}

#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("Failed {0}")]
    #[allow(dead_code)]
    Tracker(TrackerError),

    #[error("Invalid Magnet URI: {0}")]
    #[allow(dead_code)]
    InvalidMagnet(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub enum TorrentMessage {
    PeerDisconnected(Pid, Option<Bitfield>),
    PeerError(Pid, ConnectionError, Option<Bitfield>),
    ValidMetadata {
        resp: oneshot::Sender<Option<Arc<Info>>>,
    },
    DhtAddNode {
        node_addr: SocketAddr,
    },
    Have {
        #[allow(dead_code)]
        pid: Pid,
        piece_idx: u32,
    },
    ReceiveBlock(Pid, Block),
    // Peer state management
    ShouldBeInterested {
        #[allow(dead_code)]
        pid: Pid,
        bitfield: Bitfield,
        resp_tx: oneshot::Sender<bool>,
    },

    Interest(Pid),
    NotInterest(Pid),

    RequestBlock {
        pid: Pid,
        max_requests: usize,
        bitfield: Bitfield,
        block_tx: oneshot::Sender<Vec<BlockInfo>>,
    },

    RemoteBlockRequest {
        pid: Pid,
        block_info: BlockInfo,
    },

    // -- METADATA REQUEST --
    PeerWithMetadata {
        #[allow(dead_code)]
        pid: Pid,
        metadata_size: usize,
    },
    FillMetadataRequest {
        #[allow(dead_code)]
        pid: Pid,
        metadata_piece: oneshot::Sender<u32>,
    },
    ReceiveMetadata {
        #[allow(dead_code)]
        pid: Pid,
        piece_idx: u32,
        metadata: Bytes,
    },
    RejectedMetadataRequest {
        pid: Pid,
        rejected_piece: u32,
    },
    /// Inbound peer connection (peer found us and wants to connect)
    InboundPeer {
        stream: TcpStream,
        remote_addr: SocketAddr,
        supports_ext: bool,
        peer_id: PeerID,
    },
    /// Direct peer injection (for simulation testing, bypasses tracker/DHT)
    ConnectPeer {
        addr: SocketAddr,
    },
}

#[derive(Debug, Default)]
pub struct Metrics {
    pub downloaded_bytes: AtomicU64,
    pub uploaded_bytes: AtomicU64,
    /// Total number of unique peers discovered from trackers/DHT (cumulative, never decrements)
    pub peers_discovered: AtomicUsize,
}

#[derive(Debug, Clone)]
struct TrackerState {
    url: Url,
    error: Option<String>,
    last_report: Option<chrono::DateTime<chrono::Utc>>,
}

//
// TORRENT Control Structure
//

pub struct Torrent {
    info_hash: InfoHash,
    peer_id: PeerID,

    /// metadata and metadata state of the torrent
    metadata: Metadata,
    state: TorrentState,
    trackers: HashMap<Url, TrackerState>,

    tracker_client: Arc<TrackerHandler>,
    dht_client: Option<Arc<DhtHandler>>,
    storage: Arc<dyn StorageBackend>,

    metrics: Arc<Metrics>,
    peers: HashMap<Pid, PeerHandle>,
    // Torrent-local per-peer state that PeerHandle no longer holds
    pending_requests: HashMap<Pid, Vec<BlockInfo>>,

    // channels
    tx: mpsc::Sender<TorrentMessage>,
    rx: mpsc::Receiver<TorrentMessage>,

    // Download related
    piece_mananger: Option<PieceManager>,
    choker: Choker,
    //
    shutdown_rx: watch::Receiver<()>,

    bitfield: Bitfield,

    /// Directory for persisting .torrent files
    torrents_dir: PathBuf,

    /// Custom content directory for seeding (None for leeching)
    content_dir: Option<PathBuf>,

    /// Metrics sender for live updates
    ul_rate: EmaRate,
    dl_rate: EmaRate,
    progress_tx: watch::Sender<TorrentProgress>,
    /// Event sender for lifecycle changes
    event_bus: EventBus,
}

impl Torrent {
    /// Create a new Torrent from a .torrent file (with complete metadata)
    pub fn from_torrent_info(
        peer_id: PeerID,
        torrent_info: TorrentInfo,
        tracker_client: Arc<TrackerHandler>,
        dht_client: Option<Arc<DhtHandler>>,
        storage: Arc<dyn StorageBackend>,
        shutdown_rx: watch::Receiver<()>,
        torrents_dir: PathBuf,
        event_bus: crate::events::EventBus,
        unchoke_slots: usize,
    ) -> (
        Self,
        mpsc::Sender<TorrentMessage>,
        watch::Receiver<TorrentProgress>,
    ) {
        let info_hash = torrent_info.info_hash;
        let trackers = torrent_info
            .all_trackers()
            .into_iter()
            .filter_map(|url| {
                Url::parse(&url).ok().map(|u| {
                    (
                        u.clone(),
                        TrackerState {
                            url: u,
                            error: None,
                            last_report: None,
                        },
                    )
                })
            })
            .collect();

        let bitfield = Bitfield::with_size(torrent_info.num_pieces());

        let metadata = Metadata::TorrentFile(Arc::new(torrent_info));

        let (tx, rx) = mpsc::channel(CHANNEL_SIZE);

        let initial_metrics = TorrentProgress {
            name: metadata
                .display_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| info_hash.to_string()),
            total_pieces: metadata.info().map(|i| i.num_pieces() as u32).unwrap_or(0),
            total_bytes: metadata.info().map(|i| i.total_size() as u64).unwrap_or(0),
            state: crate::metrics::progress::TorrentState::Downloading,
            ..Default::default()
        };
        let (progress_tx, progress_rx) = watch::channel(initial_metrics);

        (
            Self {
                info_hash,
                peer_id,
                metadata,
                state: TorrentState::Downloading,
                trackers,
                tracker_client,
                dht_client,
                storage,
                metrics: Arc::new(Metrics::default()),
                peers: HashMap::default(),
                shutdown_rx,
                tx: tx.clone(),
                rx,
                bitfield,
                piece_mananger: None,
                choker: Choker::new(unchoke_slots),
                torrents_dir,
                content_dir: None,
                progress_tx,
                event_bus,
                pending_requests: HashMap::new(),
                ul_rate: EmaRate::default(),
                dl_rate: EmaRate::default(),
            },
            tx,
            progress_rx,
        )
    }

    /// Create a new Torrent from a magnet URI (metadata needs to be fetched)
    pub fn from_magnet(
        peer_id: PeerID,
        magnet: Magnet,
        tracker_client: Arc<TrackerHandler>,
        dht_client: Option<Arc<DhtHandler>>,
        storage: Arc<dyn StorageBackend>,
        shutdown_rx: watch::Receiver<()>,
        torrents_dir: PathBuf,
        event_bus: crate::events::EventBus,
        unchoke_slots: usize,
    ) -> (
        Self,
        mpsc::Sender<TorrentMessage>,
        watch::Receiver<TorrentProgress>,
    ) {
        let info_hash = magnet.info_hash().expect("InfoHash is a mandatory field");

        let trackers = magnet
            .trackers
            .clone()
            .into_iter()
            .map(|u| {
                (
                    u.clone(),
                    TrackerState {
                        url: u,
                        error: None,
                        last_report: None,
                    },
                )
            })
            .collect();

        let metadata = Metadata::MagnetUri {
            magnet,
            metadata_state: MetadataState::Pending,
        };

        let (tx, rx) = mpsc::channel(CHANNEL_SIZE);

        let initial_metrics = TorrentProgress {
            name: metadata
                .display_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| info_hash.to_string()),
            state: crate::metrics::progress::TorrentState::FetchingMetadata,
            ..Default::default()
        };
        let (progress_tx, progress_rx) = watch::channel(initial_metrics);

        (
            Self {
                info_hash,
                peer_id,
                metadata,
                state: TorrentState::Downloading,
                trackers,
                tracker_client,
                dht_client,
                storage,
                metrics: Arc::new(Metrics::default()),
                shutdown_rx,
                peers: HashMap::default(),
                tx: tx.clone(),
                rx,
                bitfield: Bitfield::new(),
                piece_mananger: None,
                choker: Choker::new(unchoke_slots),
                torrents_dir,
                content_dir: None,
                progress_tx,
                event_bus,
                pending_requests: HashMap::new(),
                ul_rate: EmaRate::default(),
                dl_rate: EmaRate::default(),
            },
            tx,
            progress_rx,
        )
    }

    pub fn as_seed(
        peer_id: PeerID,
        content_dir: PathBuf,
        torrent_info: TorrentInfo,
        tracker_client: Arc<TrackerHandler>,
        dht_client: Option<Arc<DhtHandler>>,
        storage: Arc<dyn StorageBackend>,
        shutdown_rx: watch::Receiver<()>,
        torrents_dir: PathBuf,
        event_bus: crate::events::EventBus,
        unchoke_slots: usize,
    ) -> (
        Self,
        mpsc::Sender<TorrentMessage>,
        watch::Receiver<TorrentProgress>,
    ) {
        let info_hash = torrent_info.info_hash;
        let trackers = torrent_info
            .all_trackers()
            .into_iter()
            .filter_map(|url| {
                Url::parse(&url).ok().map(|u| {
                    (
                        u.clone(),
                        TrackerState {
                            url: u,
                            error: None,
                            last_report: None,
                        },
                    )
                })
            })
            .collect();

        let bitfield = Bitfield::with_all_set(torrent_info.num_pieces());

        let metadata = Metadata::TorrentFile(Arc::new(torrent_info));

        let (tx, rx) = mpsc::channel(CHANNEL_SIZE);

        let initial_metrics = TorrentProgress {
            name: metadata
                .display_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| info_hash.to_string()),
            total_pieces: metadata.info().map(|i| i.num_pieces() as u32).unwrap_or(0),
            verified_pieces: metadata.info().map(|i| i.num_pieces() as u32).unwrap_or(0),
            total_bytes: metadata.info().map(|i| i.total_size() as u64).unwrap_or(0),
            downloaded_bytes: metadata.info().map(|i| i.total_size() as u64).unwrap_or(0),
            state: crate::metrics::progress::TorrentState::Seeding,
            ..Default::default()
        };
        let (progress_tx, progress_rx) = watch::channel(initial_metrics);

        (
            Self {
                info_hash,
                peer_id,
                metadata,
                state: TorrentState::Seeding,
                trackers,
                tracker_client,
                dht_client,
                storage,
                metrics: Arc::new(Metrics::default()),
                peers: HashMap::default(),
                shutdown_rx,
                tx: tx.clone(),
                rx,
                bitfield,
                piece_mananger: None,
                choker: Choker::new(unchoke_slots),
                torrents_dir,
                content_dir: Some(content_dir),
                progress_tx,
                event_bus,
                pending_requests: HashMap::new(),
                ul_rate: EmaRate::default(),
                dl_rate: EmaRate::default(),
            },
            tx,
            progress_rx,
        )
    }

    #[instrument(skip(self), name = "torrent", fields(
    info_hash=%self.info_hash,
    ))]
    pub async fn start_session(mut self) -> Result<(), TorrentError> {
        match self.state {
            TorrentState::Downloading | TorrentState::Checking | TorrentState::FetchingMetadata => {
                self.init_interal().await
            }
            TorrentState::Seeding => self.init_seed().await,
            TorrentState::Paused | TorrentState::Error(_) | TorrentState::Finished => {}
        }
        // Star announcing to Trackers/DHT
        let (discovered_peers_tx, mut discovered_peers_rx) = mpsc::channel(64);
        self.announce(&discovered_peers_tx);

        // Periodic choker tick (every 10 seconds)
        let mut choker_ticker = tokio::time::interval(Duration::from_secs(30));
        choker_ticker.tick().await;

        // Periodic metrics update (every 1 second)
        let mut metrics_ticker = tokio::time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                Ok(()) = self.shutdown_rx.changed() => {
                    tracing::info!("Shutting down...");
                    break;
                }
                maybe_msg = self.rx.recv() => {
                    match maybe_msg{
                        Some(msg) => self.handle_message(msg).await?,
                        None => break,
                    }
                }
                Some(discovered_peers) = discovered_peers_rx.recv() => {
                    for peer in &discovered_peers {
                        self.add_peer(PeerOrigin::Outbound(*peer)).await;
                    }

                }
                _ = choker_ticker.tick() => {
                    self.run_choker().await;
                }
                _ = metrics_ticker.tick() => {
                    self.update_progress();
                }
            }
        }

        Ok(())
    }

    // init bitfield
    // init piece picker
    // init piece collector
    async fn init_interal(&mut self) {
        if !self.metadata.has_metadata() {
            debug!("Should return here");
            return;
        }

        match &self.metadata {
            Metadata::TorrentFile(torrent_info) => {
                if let Err(e) = self
                    .storage
                    .add_torrent(self.info_hash, torrent_info.info.clone())
                    .await
                {
                    tracing::error!("Failed to register torrent with storage: {}", e);
                    return;
                }

                let info = torrent_info.info.clone();
                self.bitfield = Bitfield::with_size(info.pieces.len());
                self.piece_mananger = Some(PieceManager::new(info));
            }

            Metadata::MagnetUri {
                metadata_state: MetadataState::Complete(info),
                ..
            } => {
                if let Err(e) = self.storage.add_torrent(self.info_hash, info.clone()).await {
                    tracing::error!("Failed to register torrent with storage: {}", e);
                    return;
                }
                self.bitfield = Bitfield::with_size(info.pieces.len());

                self.piece_mananger = Some(PieceManager::new(info.clone()));

                if let Some((_, torrent_bytes)) = self.metadata.to_torrent_file() {
                    let torrent_path = self
                        .torrents_dir
                        .join(format!("{}.torrent", self.info_hash));
                    if let Err(e) = std::fs::write(&torrent_path, &torrent_bytes) {
                        tracing::warn!(
                            "Failed to persist .torrent file to {}: {}",
                            torrent_path.display(),
                            e
                        );
                    } else {
                        tracing::info!("Persisted .torrent file to {}", torrent_path.display());
                    }
                }
            }
            Metadata::MagnetUri { .. } => {
                panic!("called this in wrong state")
            }
        }
    }

    async fn init_seed(&mut self) {
        let Some(content_dir) = &self.content_dir else {
            tracing::error!("Cannot seed without content_dir");
            return;
        };

        let info = match &self.metadata {
            Metadata::TorrentFile(torrent_info) => torrent_info.info.clone(),
            Metadata::MagnetUri { .. } => {
                tracing::error!("Cannot seed from magnet URI - need complete metadata");
                return;
            }
        };

        if let Err(e) = self
            .storage
            .add_seed(self.info_hash, info.clone(), content_dir.clone())
            .await
        {
            tracing::error!("Failed to register seed with storage: {}", e);
            return;
        }

        self.piece_mananger = Some(PieceManager::new_all_have(info));
    }

    pub async fn add_peer(&mut self, peer: PeerOrigin) {
        // check if we already are connected to this peer?
        let already_connected = self.peers.values().any(|p| p.peer_addr == *peer.get_addr());
        if already_connected {
            return;
        }

        if self.peers.len() >= 500 {
            return;
        }

        let pid = Pid(PEER_COUNTER.fetch_add(1, Ordering::Relaxed));
        self.metrics
            .peers_discovered
            .fetch_add(1, Ordering::Relaxed);

        let direction = match &peer {
            PeerOrigin::Inbound { .. } => crate::events::peer::Direction::Inbound,
            PeerOrigin::Outbound(_) => crate::events::peer::Direction::Outbound,
        };
        let peer_handle = match peer {
            PeerOrigin::Inbound {
                peer_id: remote_peer_id,
                stream,
                supports_ext,
                remote_addr,
            } => spawn_inbound(
                pid,
                remote_addr,
                stream,
                self.info_hash,
                self.tx.clone(),
                remote_peer_id,
                supports_ext,
            ),
            PeerOrigin::Outbound(remote_addr) => spawn_outbound(
                pid,
                remote_addr,
                self.info_hash,
                self.peer_id,
                self.tx.clone(),
            ),
        };

        // Send our bitfield if we have any pieces
        if self.bitfield.size() > 0 && self.bitfield.iter_set().next().is_some() {
            peer_handle
                .tx
                .send(PeerMessage::SendBitfield {
                    bitfield: self.bitfield.clone(),
                })
                .await
                .unwrap();
        }

        self.pending_requests.insert(pid, Vec::new());
        self.peers.insert(pid, peer_handle.clone());
        let _ = self
            .event_bus
            .peer_tx
            .send(crate::events::peer::PeerEvent::Connected {
                addr: peer_handle.peer_addr,
                direction,
            });
    }

    async fn handle_message(&mut self, msg: TorrentMessage) -> Result<(), TorrentError> {
        match msg {
            TorrentMessage::RemoteBlockRequest { pid, block_info } => {
                let Some(peer) = self.peers.get(&pid) else {
                    tracing::debug!("Peer {} disconnected before block could be served", pid);
                    return Ok(());
                };

                match self.storage.read_block(self.info_hash, block_info).await {
                    Ok(block) => {
                        let block_len = block.data.len() as u64;
                        if let Err(e) = peer
                            .tx
                            .send(PeerMessage::SendMessage(Message::Piece(block)))
                            .await
                        {
                            tracing::warn!("Failed to send piece to peer {}: {}", pid, e);
                        } else {
                            self.ul_rate.record(block_len);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to read block {} offset {} for peer {}: {}",
                            block_info.index,
                            block_info.begin,
                            pid,
                            e
                        );
                    }
                }
            }
            TorrentMessage::PeerDisconnected(pid, bitfield) => {
                tracing::debug!("PeerDisconnected");
                self.clean_up_peer(pid, bitfield).await
            }
            TorrentMessage::PeerError(pid, _err, bitfield) => {
                tracing::debug!("PeerError");
                self.clean_up_peer(pid, bitfield).await;
            }
            TorrentMessage::Have { pid: _, piece_idx } => {
                tracing::debug!("peer SEND HAVE");
                let p = self.piece_mananger.as_mut().expect("init");
                p.increment_availability(&AvailabilityUpdate::Have(piece_idx));
            }
            TorrentMessage::ReceiveBlock(pid, block) => {
                tracing::debug!("Incoming block {block:?}");
                self.incoming_block(pid, block).await;
            }
            TorrentMessage::Interest(pid) => {
                tracing::debug!("Peer {pid:?} is interested in our pieces");
                let should_unchoke = self.choker.on_peer_interested(pid);
                if should_unchoke {
                    self.send_to_peer(pid, PeerMessage::SendUnchoke);
                    tracing::debug!("Unchoked peer {pid:?}");
                }
            }
            TorrentMessage::NotInterest(pid) => {
                tracing::debug!("Peer {pid:?} is no longer interested in our pieces");
                let was_unchoked = self.choker.on_peer_not_interested(pid);
                if was_unchoked {
                    self.send_to_peer(pid, PeerMessage::SendChoke);
                    tracing::debug!("Choked peer {pid:?}");
                }
            }
            TorrentMessage::ShouldBeInterested {
                pid: _,
                bitfield,
                resp_tx,
            } => {
                debug!("---------- received a should be interested");
                // // debug!(?bitfield);
                self.piece_mananger
                    .as_mut()
                    .expect("initialized on start")
                    .increment_availability(&AvailabilityUpdate::Bitfield(&bitfield));

                //
                let interest = bitfield
                    .iter_set()
                    .any(|piece_peer_has| !self.bitfield.has(piece_peer_has));
                let _ = resp_tx.send(interest);
            }
            TorrentMessage::RequestBlock {
                pid,
                max_requests,
                bitfield,
                block_tx,
            } => {
                tracing::debug!("----received block request");
                let p = self.piece_mananger.as_mut().expect("init");
                let blocks = p.pick_piece(&bitfield, max_requests);
                //
                // Register these requests (increment heat)
                for block in &blocks {
                    let request = BlockRequest::from(*block);
                    p.add_request(request);
                }

                self.pending_requests
                    .get_mut(&pid)
                    .unwrap()
                    .extend_from_slice(&blocks);

                let _ = block_tx.send(blocks);
            }
            TorrentMessage::ValidMetadata { resp } => {
                let metadata = self.metadata.info();
                let _ = resp.send(metadata);
            }
            TorrentMessage::PeerWithMetadata {
                pid: _,
                metadata_size,
            } => {
                tracing::debug!("Received metadata size: {} bytes", metadata_size);

                // Use the new set_metadata_size method to properly initialize fetching state
                if let Err(e) = self.metadata.set_metadata_size(metadata_size) {
                    tracing::debug!("Failed to set metadata size: {}", e);
                }
            }
            TorrentMessage::FillMetadataRequest {
                pid: _, // maybe use this to mark the peer as participant in the construction of
                // metadata, and use it to penalize it in the case of a info hash mismatch
                metadata_piece,
            } => {
                if let Some(piece) = self.metadata.get_piece() {
                    let _ = metadata_piece.send(piece);
                } else {
                    debug!("nothing to request");
                }
            }
            TorrentMessage::ReceiveMetadata {
                pid: _,
                metadata,
                piece_idx,
            } => {
                tracing::debug!(
                    "Received metadata piece {} ({} bytes)",
                    piece_idx,
                    metadata.len()
                );

                // Put the metadata piece into the buffer
                if let Err(e) = self.metadata.put_metadata_piece(metadata, piece_idx) {
                    tracing::debug!("Failed to put metadata piece {}: {}", piece_idx, e);
                    return Ok(());
                }

                // Mark the piece as received and check if we have all pieces
                match self.metadata.mark_metadata_piece(piece_idx) {
                    Ok(true) => {
                        tracing::info!("All metadata pieces received, constructing info...");
                        self.on_complete_metadata().await?;
                    }
                    Ok(false) => {
                        // Still need more pieces, continue requesting
                        tracing::debug!(
                            "---------------- Metadata piece {} marked, still need more pieces",
                            piece_idx
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Failed to mark metadata piece {}: {}", piece_idx, e);
                    }
                }
            }
            TorrentMessage::RejectedMetadataRequest {
                pid,
                rejected_piece,
            } => {
                debug!("{:?} rejected metadata request", pid);
                if let Err(e) = self.metadata.metadata_request_reject(rejected_piece) {
                    warn!(?e);
                }
            }
            TorrentMessage::DhtAddNode { node_addr } => {
                if let Some(dht_client) = self.dht_client.as_ref() {
                    let dht_client = dht_client.clone();
                    tokio::task::spawn(async move {
                        if let Err(e) = dht_client.try_add_node(node_addr).await {
                            warn!(?e);
                        }
                    });
                }
            }
            TorrentMessage::InboundPeer {
                stream,
                remote_addr,
                supports_ext,
                peer_id,
            } => {
                info!("Received inbound peer connection from {}", remote_addr);
                self.add_peer(PeerOrigin::Inbound {
                    stream,
                    remote_addr,
                    supports_ext,
                    peer_id,
                })
                .await;
            }
            TorrentMessage::ConnectPeer { addr } => {
                info!("Injected outbound peer connection to {}", addr);
                self.add_peer(PeerOrigin::Outbound(addr)).await;
            }
        }
        Ok(())
    }

    async fn incoming_block(&mut self, pid: Pid, block: Block) {
        let request = BlockRequest {
            piece_index: block.index,
            begin: block.begin,
            length: u32::try_from(block.data.len()).expect("incoming block length > u32::MAX"),
        };

        // Decrement heat
        if let Some(p) = self.piece_mananger.as_mut() {
            info!("DELETING REQUESTS");
            p.delete_request(request);
        }

        if let Some(reqs) = self.pending_requests.get_mut(&pid) {
            reqs.retain(|r| *r != BlockInfo::from(request));
        }

        let pids_to_cancel: Vec<Pid> = self
            .pending_requests
            .iter()
            .filter(|(_, reqs)| reqs.contains(&BlockInfo::from(request)))
            .map(|(p, _)| *p)
            .collect();

        info!("SEND CANCELS");
        for cancel_pid in pids_to_cancel {
            self.send_to_peer(
                cancel_pid,
                PeerMessage::SendMessage(Message::Cancel(BlockInfo::from(request))),
            )
        }
        info!("END OF SEND CANCELS");

        if let Some(piece) = self
            .piece_mananger
            .as_mut()
            .expect("state initializated")
            .add_block(block)
        {
            info!("CALLING ON COMPLETE PIECE");
            self.on_complete_piece(request.piece_index, piece).await;
            info!("RETURN FROM COMPLETE PIECE");
        }
    }

    async fn on_complete_piece(&mut self, piece_index: u32, piece: Box<[u8]>) {
        // Mark piece as downloaded first
        debug!("MARKING PIECE AS DOWNLOADED");
        self.piece_mananger
            .as_mut()
            .expect("initialized")
            .set_piece_as(piece_index as usize, PieceState::Downloaded);

        let torrent_id = self.info_hash;
        let piece: Arc<[u8]> = piece.into();

        debug!("STARTING VERIFICATION");
        // Verify piece hash
        let valid = match self
            .storage
            .verify_piece(torrent_id, piece_index, piece.clone())
            .await
        {
            Ok(valid) => valid,
            Err(e) => {
                tracing::error!(
                    "Failed to verify piece {}: {} - resetting for re-download",
                    piece_index,
                    e
                );
                let _ = self
                    .event_bus
                    .torrent_tx
                    .send(crate::events::torrent::TorrentEvent::HashFailed { piece_index });
                self.piece_mananger
                    .as_mut()
                    .expect("initialized")
                    .reset_piece(piece_index as usize);
                return;
            }
        };

        debug!("FINISHED VERIFICATION");

        if !valid {
            tracing::warn!(
                "Piece {} failed hash verification - resetting for re-download",
                piece_index
            );
            counters::piece_failed();
            let _ = self
                .event_bus
                .torrent_tx
                .send(crate::events::torrent::TorrentEvent::HashFailed { piece_index });
            self.piece_mananger
                .as_mut()
                .expect("initialized")
                .reset_piece(piece_index as usize);
            return;
        }

        counters::piece_passed();
        // Mark as have and broadcast
        self.piece_mananger
            .as_mut()
            .expect("initialized")
            .set_piece_as(piece_index as usize, PieceState::Have);
        self.bitfield.set(piece_index as usize);

        let piece_len = piece.len() as u64;
        self.metrics
            .downloaded_bytes
            .fetch_add(piece_len, Ordering::Relaxed);
        self.dl_rate.record(piece_len);

        debug!("MARKED AS HAVE");
        tracing::debug!("BROADCASTING PIECE TO PEERS");
        self.broadcast_to_peers(PeerMessage::SendHave { piece_index });
        tracing::debug!("RETURNING FROM BROADCASTING PIECE TO PEERS");

        // Write piece to disk
        tracing::debug!("GOING TO WRITE PIECE");
        if let Err(e) = self
            .storage
            .write_piece(torrent_id, piece_index, piece)
            .await
        {
            tracing::error!(
                "Failed to write piece {}: {} - resetting for re-download",
                piece_index,
                e
            );
            // Reset piece so it will be re-downloaded
            self.piece_mananger
                .as_mut()
                .expect("initialized")
                .reset_piece(piece_index as usize);
        }

        tracing::debug!("RETURNED FROM TO WRITE PIECE");

        if self
            .piece_mananger
            .as_ref()
            .expect("initalized")
            .have_all_pieces()
        {
            let prev = self.state.clone();
            self.state = TorrentState::Seeding;

            let prev_metric = match prev {
                TorrentState::FetchingMetadata => {
                    crate::metrics::progress::TorrentState::FetchingMetadata
                }
                TorrentState::Seeding => crate::metrics::progress::TorrentState::Seeding,
                TorrentState::Paused => crate::metrics::progress::TorrentState::Paused,
                TorrentState::Downloading => crate::metrics::progress::TorrentState::Downloading,
                _ => prev.clone(),
            };
            let _ = self.event_bus.torrent_tx.send(
                crate::events::torrent::TorrentEvent::StateChanged {
                    prev: prev_metric,
                    next: crate::metrics::progress::TorrentState::Seeding,
                },
            );
            let _ = self
                .event_bus
                .torrent_tx
                .send(crate::events::torrent::TorrentEvent::TorrentFinished);

            if let Some(name) = self.metadata.display_name() {
                info!("Torrent-{} Download Completed", name);
            } else {
                info!("Torrent Download Completed");
            }
            let _ = self
                .event_bus
                .session_tx
                .send(SessionEvent::TorrentCompleted(self.info_hash));
        }
    }

    // 1.Register torrent in storage
    // 2.Build bitfield for this torrent
    // 3.Notify peer connection control structure about have metainfo for ending ut_metadfata
    // fetching
    async fn on_complete_metadata(&mut self) -> Result<(), TorrentError> {
        if let Err(e) = self.metadata.construct_info() {
            tracing::error!("Failed to construct info from metadata: {}", e);
        } else {
            self.init_interal().await;

            let info = self
                .metadata
                .info()
                .expect("metadata was not successfully constructed");

            // tracing::info!("Metadata Info: {:#?}", info);

            self.broadcast_to_peers(PeerMessage::HaveMetadata(info));
            let _ = self
                .event_bus
                .session_tx
                .send(SessionEvent::MetadataFetched(self.info_hash));
        }

        Ok(())
    }

    async fn clean_up_peer(&mut self, pid: Pid, bitfield: Option<Bitfield>) {
        if let Some(unchoked_pid) = self.choker.on_peer_disconnected(pid) {
            self.send_to_peer(unchoked_pid, PeerMessage::SendUnchoke);
        }

        if let Some(peer_handle) = self.peers.remove(&pid) {
            let _ = self
                .event_bus
                .peer_tx
                .send(crate::events::peer::PeerEvent::Disconnected {
                    addr: peer_handle.peer_addr,
                    reason: crate::events::peer::DisconnectReason::Other("Clean up".to_string()),
                });
        }

        let requests: Vec<_> = self
            .pending_requests
            .remove(&pid)
            .unwrap_or_default()
            .into_iter()
            .map(BlockRequest::from)
            .collect();

        if let Some(bitfield) = bitfield
            && let Some(manager) = self.piece_mananger.as_mut()
        {
            manager.decrement_availability(&AvailabilityUpdate::Bitfield(&bitfield));
            manager.cancel_peer_requests(&requests);
        }
    }

    /// Run the choker algorithm periodically to rotate upload slots
    async fn run_choker(&mut self) {
        let (to_choke, to_unchoke) = self.choker.re_evaluate_unchokes();

        // Apply choke decisions
        for pid in to_choke {
            self.send_to_peer(pid, PeerMessage::SendChoke);
            if let Some(peer) = self.peers.get(&pid) {
                let _ = self
                    .event_bus
                    .peer_tx
                    .send(crate::events::peer::PeerEvent::Choked {
                        addr: peer.peer_addr,
                    });
            }
            tracing::debug!("Periodic choker: choked peer {pid:?}");
        }

        // Apply unchoke decisions
        for pid in to_unchoke {
            self.send_to_peer(pid, PeerMessage::SendUnchoke);
            if let Some(peer) = self.peers.get(&pid) {
                let _ = self
                    .event_bus
                    .peer_tx
                    .send(crate::events::peer::PeerEvent::Unchoked {
                        addr: peer.peer_addr,
                    });
            }
            tracing::debug!("Periodic choker: unchoked peer {pid:?}");
        }
    }

    fn update_progress(&mut self) {
        self.dl_rate.update();
        self.ul_rate.update();

        let progress = if let Some(mananger) = &self.piece_mananger {
            mananger.get_progress()
        } else {
            0.0
        };

        let metrics = TorrentProgress {
            name: self
                .metadata
                .display_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| self.info_hash.to_string()),
            total_pieces: self
                .metadata
                .info()
                .map(|i| i.num_pieces() as u32)
                .unwrap_or(0),
            verified_pieces: {
                let total = self
                    .metadata
                    .info()
                    .map(|i| i.num_pieces() as f64)
                    .unwrap_or(0.0);
                (progress * total) as u32
            },
            failed_pieces: 0,
            total_bytes: self
                .metadata
                .info()
                .map(|i| i.total_size() as u64)
                .unwrap_or(0),
            downloaded_bytes: self.metrics.downloaded_bytes.load(Ordering::Relaxed),
            uploaded_bytes: self.metrics.uploaded_bytes.load(Ordering::Relaxed),
            connected_peers: self.peers.len() as u32,
            download_rate: self.dl_rate.rate(),
            upload_rate: self.ul_rate.rate(),
            state: self.state.clone(),
            eta_seconds: None,
        };

        let _ = self.progress_tx.send(metrics);
    }

    // TODO: Message delivery strategy — not all messages warrant blocking send.
    // try_send (non-blocking, drop on full):
    //   - SendHave, SendChoke, SendUnchoke: peer will learn state eventually or disconnect
    //   - SendBitfield, HaveMetadata: one-time setup, stale if peer is lagging anyway
    //   - Disconnect: if channel is full the peer is stalled and will be cleaned up by heartbeat
    // send_async (blocking) or try_send with timeout:
    //   - SendMessage(Piece): actual upload data, worth waiting briefly for delivery
    // A peer with a persistently full channel will naturally disconnect via the
    // heartbeat timeout in PeerConnection — try_send drops accelerate that cleanup
    // rather than holding up the torrent loop waiting on a dead peer.
    fn send_to_peer(&self, pid: Pid, message: PeerMessage) {
        if let Some(p) = self.peers.get(&pid) {
            match p.tx.try_send(message) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(%pid, "peer channel full, dropping message");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Task already exited, PeerDisconnected is in flight
                }
            }
        }
    }

    /// Send a message to a specific peer
    fn broadcast_to_peers(&self, message: PeerMessage) {
        for pid in self.peers.keys() {
            self.send_to_peer(*pid, message.clone());
        }
    }

    /// Send a protocol message to a specific peer
    #[allow(dead_code)]
    fn send_message_to_peer(&self, pid: Pid, message: Message) {
        self.send_to_peer(pid, PeerMessage::SendMessage(message))
    }

    // If we are seeding a file we are not interest in receiving peers
    fn announce(&self, discovered_peers_tx: &mpsc::Sender<Vec<SocketAddr>>) {
        let client_state = self.metadata.info().map_or_else(
            || ClientState::new(0, 0, 0, Events::Started),
            |info| {
                let event = match self.state {
                    TorrentState::Seeding => Events::Completed,
                    TorrentState::Downloading => Events::Started,
                    _ => Events::None,
                };
                ClientState::new(
                    0,
                    info.piece_length
                        * i64::try_from(info.pieces.len())
                            .expect("incoming info length > i64::MAX"),
                    0,
                    event,
                )
            },
        );

        // Spawn tracker announce tasks
        for announce_url in self.trackers.keys() {
            let tracker_client = self.tracker_client.clone();
            let info_hash = self.info_hash;
            let seed = self.state == TorrentState::Seeding;
            // TODO: Why use .to_string()?
            let announce = announce_url.to_string();
            let discovered_peers_tx = discovered_peers_tx.clone();
            let _torrent_tx = self.tx.clone();
            let event_bus_tx = self.event_bus.torrent_tx.clone();
            tokio::spawn(async move {
                loop {
                    let response = tracker_client
                        .announce(info_hash, announce.clone(), client_state)
                        .await;

                    if let Err(e) = response {
                        tracing::warn!("Failed to announce to tracker {}: {}", announce, e);
                        let _ =
                            event_bus_tx.send(crate::events::torrent::TorrentEvent::TrackerError {
                                url: announce.clone(),
                                error: format!("{:?}", e),
                                times_in_row: 1,
                            });
                        return;
                    }

                    let Ok(response) = response else {
                        tracing::warn!("Tracker failure");
                        break;
                    };

                    let _ =
                        event_bus_tx.send(crate::events::torrent::TorrentEvent::TrackerAnnounced {
                            url: announce.clone(),
                            peers_received: response.peers.len() as u32,
                        });

                    let sleep_duration = response.interval;
                    if !seed {
                        let _ = discovered_peers_tx.send(response.peers).await;
                    }

                    sleep(Duration::from_secs(
                        u64::try_from(sleep_duration)
                            .expect("tracker interval must be  non-negative"),
                    ))
                    .await;
                }
            });
        }

        // Spawn DHT discovery task (runs in parallel with tracker announces)
        // TODO: Announce method has an skectchy impl that only tries to announce in order to
        // get_peers i need to read in detail BEP-5 to announce we are actually seeding a file
        if let Some(dht) = self.dht_client.clone()
            && self.state != TorrentState::Seeding
        {
            let info_hash = self.info_hash;
            let discovered_peers_tx = discovered_peers_tx.clone();
            let port = 6881_u16; // TODO: Use actual listening port from session

            tokio::spawn(async move {
                // DHT re-announce interval (15 minutes as per BEP 5 recommendation)
                const DHT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(15 * 60);

                const MAX_RESPONSES: usize = 50;

                let _ = dht.announce_peer(info_hash, port).await;

                loop {
                    let mut discovered_peers_stream = dht.get_peers(info_hash).await;
                    let mut response_count = 0;

                    while let Some(peers) = discovered_peers_stream.recv().await {
                        let _ = discovered_peers_tx.send(peers).await;
                        response_count += 1;
                        if response_count >= MAX_RESPONSES {
                            break;
                        }
                    }

                    // Wait before next DHT announce
                    sleep(DHT_ANNOUNCE_INTERVAL).await;
                }
            });
        }
    }
}
