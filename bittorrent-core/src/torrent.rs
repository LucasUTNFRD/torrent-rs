use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use bittorrent_common::{
    metainfo::{Info, TorrentInfo},
    types::InfoHash,
};
use bytes::Bytes;
use magnet_uri::Magnet;
use mainline_dht::DhtHandler;
use peer_protocol::protocol::{Block, BlockInfo, Message};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot, watch},
    time::sleep,
};
use tracing::{debug, field::debug, info, instrument, warn};
use tracker_client::{ClientState, Events, TrackerError, TrackerHandler, TrackerResponse};
use url::Url;

use crate::{
    Storage,
    bitfield::Bitfield,
    choker::Choker,
    metadata::{Metadata, MetadataState},
    peer::{
        PeerMessage, PeerState,
        metrics::PeerMetrics,
        peer_connection::{ConnectionError, spawn_inbound_peer, spawn_outgoing_peer},
    },
    piece_picker::{AvailabilityUpdate, BlockRequest, PieceManager, PieceState},
};

// Peer related
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Pid(pub usize);

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}
static PEER_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub enum PeerSource {
    Inbound {
        stream: TcpStream,
        remote_addr: SocketAddr,
        supports_ext: bool,
    },
    Outbound(SocketAddr), // Socket to connect + Our peer id
}

impl PeerSource {
    pub const fn get_addr(&self) -> &SocketAddr {
        match self {
            Self::Inbound {
                stream: _,
                remote_addr,
                supports_ext: _,
            }
            | Self::Outbound(remote_addr) => remote_addr,
        }
    }
}
//
#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("Failed {0}")]
    #[allow(dead_code)]
    Tracker(TrackerError),

    #[error("Invalid Magnet URI: {0}")]
    InvalidMagnet(String),
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
        pid: Pid,
        piece_idx: u32,
    },
    ReceiveBlock(Pid, peer_protocol::protocol::Block),
    // Peer state management
    ShouldBeInterested {
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
        pid: Pid,
        metadata_size: usize,
    },
    FillMetadataRequest {
        pid: Pid,
        metadata_piece: oneshot::Sender<u32>,
    },
    ReceiveMetadata {
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
    },
    /// Get torrent statistics
    GetStats {
        resp: oneshot::Sender<TorrentStats>,
    },
}

/// Statistics for a torrent
#[derive(Debug, Clone)]
pub struct TorrentStats {
    pub state: TorrentState,
    pub progress: f64,
    pub download_rate: u64,
    pub upload_rate: u64,
    pub peers_connected: usize,
    pub peers_discovered: usize,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
}

//
// Metrics Control Structure
//

#[derive(Debug, Default)]
pub struct Metrics {
    pub downloaded_bytes: AtomicU64,
    pub uploaded_bytes: AtomicU64,
    /// Total number of unique peers discovered from trackers/DHT (cumulative, never decrements)
    pub peers_discovered: AtomicUsize,
}

//
// TORRENT Control Structure
//

/// Torrent Struct for individual torrent files
pub struct Torrent {
    info_hash: InfoHash,

    /// metadata and metadata state of the torrent
    metadata: Metadata,
    state: TorrentState,
    trackers: Vec<Url>,

    tracker_client: Arc<TrackerHandler>,
    dht_client: Option<Arc<DhtHandler>>,
    storage: Arc<Storage>,

    metrics: Arc<Metrics>,
    peers: HashMap<Pid, PeerState>,

    // channels
    tx: mpsc::Sender<TorrentMessage>,
    rx: mpsc::Receiver<TorrentMessage>,

    // Download related
    piece_mananger: Option<PieceManager>,
    choker: Choker,
    //
    shutdown_rx: watch::Receiver<()>,

    bitfield: Bitfield,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TorrentState {
    Seeding,
    Paused,
    Leeching,
}

impl Torrent {
    /// Create a new Torrent from a .torrent file (with complete metadata)
    pub fn from_torrent_info(
        torrent_info: TorrentInfo,
        tracker_client: Arc<TrackerHandler>,
        dht_client: Option<Arc<DhtHandler>>,
        storage: Arc<Storage>,
        shutdown_rx: watch::Receiver<()>,
    ) -> (Self, mpsc::Sender<TorrentMessage>) {
        let info_hash = torrent_info.info_hash;
        let trackers = torrent_info
            .all_trackers()
            .into_iter()
            .filter_map(|url| Url::parse(&url).ok())
            .collect();

        let bitfield = Bitfield::with_size(torrent_info.num_pieces());

        let metadata = Metadata::TorrentFile(Arc::new(torrent_info));

        let (tx, rx) = mpsc::channel(64);

        (
            Self {
                info_hash,
                metadata,
                state: TorrentState::Leeching,
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
                choker: Choker::new(4), // Default 4 upload slots
            },
            tx,
        )
    }

    /// Create a new Torrent from a magnet URI (metadata needs to be fetched)
    pub fn from_magnet(
        magnet: Magnet,
        tracker_client: Arc<TrackerHandler>,
        dht_client: Option<Arc<DhtHandler>>,
        storage: Arc<Storage>,
        shutdown_rx: watch::Receiver<()>,
    ) -> (Self, mpsc::Sender<TorrentMessage>) {
        let info_hash = magnet.info_hash().expect("InfoHash is a mandatory field");

        let trackers = magnet.trackers.clone();

        let metadata = Metadata::MagnetUri {
            magnet,
            metadata_state: MetadataState::Pending,
        };

        let (tx, rx) = mpsc::channel(300);

        (
            Self {
                info_hash,
                metadata,
                state: TorrentState::Leeching,
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
                choker: Choker::new(4), // Default 4 upload slots
            },
            tx,
        )
    }

    #[instrument(skip(self), name = "torrent", fields(
    info_hash=%self.info_hash,
    ))]
    pub async fn start_session(mut self) -> Result<(), TorrentError> {
        self.init_interal().await;

        // Star announcing to Trackers/DHT
        let (discovered_peers_tx, mut discovered_peers_rx) = mpsc::channel(16);
        self.announce(&discovered_peers_tx);

        // Periodic choker tick (every 10 seconds)
        let mut choker_ticker = tokio::time::interval(Duration::from_secs(10));
        choker_ticker.tick().await;

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
                        self.add_peer(PeerSource::Outbound(*peer));
                    }

                }
                _ = choker_ticker.tick() => {
                    self.run_choker().await;
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
            debug("Should return here");
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
            }
            Metadata::MagnetUri { .. } => {
                panic!("called this in wrong state")
            }
        }
    }

    // TODO: implement a retry mechanism for failed peer
    pub fn add_peer(&mut self, peer: PeerSource) {
        // check if we already are connected to this peer?
        let already_connected = self.peers.values().any(|p| p.addr == *peer.get_addr());
        if already_connected {
            return;
        }

        if self.peers.len() >= 500 {
            // TODO: This is silently discarding peers we NEED a retry-mechanism
            return;
        }

        // Create peer info
        let peer_id = PEER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let peer_id = Pid(peer_id);

        let (peer_tx, peer_rx) = mpsc::channel(64);

        let peer_addr = *peer.get_addr();

        self.metrics
            .peers_discovered
            .fetch_add(1, Ordering::Relaxed);

        // Create shared metrics that both torrent and peer connection will use
        let shared_metrics = Arc::new(PeerMetrics::new());
        let metrics_clone = shared_metrics.clone();

        match peer {
            PeerSource::Inbound {
                stream,
                remote_addr,
                supports_ext,
            } => spawn_inbound_peer(
                peer_id,
                remote_addr,
                stream,
                supports_ext,
                self.info_hash,
                self.tx.clone(),
                peer_rx,
            ),
            PeerSource::Outbound(remote_addr) => spawn_outgoing_peer(
                peer_id,
                remote_addr,
                self.info_hash,
                self.tx.clone(),
                peer_rx,
            ),
        }

        // Send the shared metrics to the peer connection
        let _ = peer_tx.try_send(PeerMessage::Connected {
            metrics: metrics_clone,
        });

        let peer = PeerState {
            addr: peer_addr,
            tx: peer_tx,
            metrics: shared_metrics,
            pending_requests: Vec::new(),
        };

        self.peers.insert(peer_id, peer);
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
                        // Send piece to peer (upload tracking happens in peer_connection)
                        if let Err(e) = peer
                            .tx
                            .send(PeerMessage::SendMessage(Message::Piece(block)))
                            .await
                        {
                            tracing::warn!("Failed to send piece to peer {}: {}", pid, e);
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
                self.clean_up_peer(pid, bitfield).await
            }
            TorrentMessage::PeerError(pid, err, bitfield) => {
                self.clean_up_peer(pid, bitfield).await;
            }
            TorrentMessage::Have { pid, piece_idx } => {
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
                    self.send_to_peer(pid, PeerMessage::SendUnchoke).await;
                    tracing::debug!("Unchoked peer {pid:?}");
                }
            }
            TorrentMessage::NotInterest(pid) => {
                tracing::debug!("Peer {pid:?} is no longer interested in our pieces");
                let was_unchoked = self.choker.on_peer_not_interested(pid);
                if was_unchoked {
                    self.send_to_peer(pid, PeerMessage::SendChoke).await;
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
                // debug!("----recevied blokc request");
                let p = self.piece_mananger.as_mut().expect("init");
                let blocks = p.pick_piece(&bitfield, max_requests);
                //
                // Register these requests (increment heat)
                for block in &blocks {
                    let request = BlockRequest::from(*block);
                    p.add_request(request);
                }

                self.peers
                    .get_mut(&pid)
                    .unwrap()
                    .pending_requests
                    .extend(blocks.clone());

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
                pid, // maybe use this to mark the peer as participant in the construction of
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
            } => {
                info!("Received inbound peer connection from {}", remote_addr);
                self.add_peer(PeerSource::Inbound {
                    stream,
                    remote_addr,
                    supports_ext,
                });
            }
            TorrentMessage::GetStats { resp } => {
                let stats = self.get_stats();
                let _ = resp.send(stats);
            }
        }
        Ok(())
    }

    /// Get current torrent statistics
    fn get_stats(&self) -> TorrentStats {
        // Aggregate peer metrics
        let mut total_download_rate = 0u64;
        let mut total_upload_rate = 0u64;
        let mut downloaded_bytes = 0u64;
        let mut uploaded_bytes = 0u64;

        for peer in self.peers.values() {
            total_download_rate += peer.metrics.get_download_rate();
            total_upload_rate += peer.metrics.get_upload_rate();
            downloaded_bytes += peer.metrics.get_bytes_downloaded();
            uploaded_bytes += peer.metrics.get_bytes_uploaded();
        }

        let progress = self
            .piece_mananger
            .as_ref()
            .map_or(0.0, PieceManager::get_progress);

        // Determine state based on progress and piece manager state
        let state = if self.metadata.has_metadata() {
            if progress >= 1.0 {
                TorrentState::Seeding
            } else {
                TorrentState::Leeching
            }
        } else {
            // No metadata yet, still fetching
            TorrentState::Leeching
        };

        TorrentStats {
            state,
            progress,
            download_rate: total_download_rate,
            upload_rate: total_upload_rate,
            peers_connected: self.peers.len(),
            peers_discovered: self.metrics.peers_discovered.load(Ordering::Relaxed),
            downloaded_bytes,
            uploaded_bytes,
        }
    }

    async fn incoming_block(&mut self, pid: Pid, block: Block) {
        let piece_index = block.index;

        let request = BlockRequest {
            piece_index: block.index,
            begin: block.begin,
            length: u32::try_from(block.data.len()).expect("incoming block length > u32::MAX"),
        };

        // Decrement heat
        if let Some(p) = self.piece_mananger.as_mut() {
            p.delete_request(request);
        }

        // Remove from peer's pending requests
        if let Some(peer_state) = self.peers.get_mut(&pid) {
            peer_state
                .pending_requests
                .retain(|r| *r != BlockInfo::from(request));
        }

        let pids_to_broadcast_cancel: Vec<_> = self
            .peers
            .iter()
            .filter(|(_pid, state)| state.pending_requests.contains(&BlockInfo::from(request)))
            .map(|(p, _s)| p)
            .collect();

        for pid in pids_to_broadcast_cancel {
            debug!("Block {request:?} requested by many peers sending cancel to others");
            self.send_to_peer(
                *pid,
                PeerMessage::SendMessage(Message::Cancel(BlockInfo::from(request))),
            )
            .await;
        }

        if let Some(piece) = self
            .piece_mananger
            .as_mut()
            .expect("state initializated")
            .add_block(block)
        {
            self.on_complete_piece(piece_index, piece).await;
        }
    }

    async fn on_complete_piece(&mut self, piece_index: u32, piece: Box<[u8]>) {
        // Mark piece as downloaded first
        self.piece_mananger
            .as_mut()
            .expect("initialized")
            .set_piece_as(piece_index as usize, PieceState::Downloaded);

        let torrent_id = self.info_hash;
        let piece: Arc<[u8]> = piece.into();

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
                self.piece_mananger
                    .as_mut()
                    .expect("initialized")
                    .reset_piece(piece_index as usize);
                return;
            }
        };

        if !valid {
            tracing::warn!(
                "Piece {} failed hash verification - resetting for re-download",
                piece_index
            );
            self.piece_mananger
                .as_mut()
                .expect("initialized")
                .reset_piece(piece_index as usize);
            return;
        }

        // Mark as have and broadcast
        self.piece_mananger
            .as_mut()
            .expect("initialized")
            .set_piece_as(piece_index as usize, PieceState::Have);
        self.bitfield.set(piece_index as usize);
        // tracing::debug!("BROADCASTING PIECE TO PEERS");
        self.broadcast_to_peers(PeerMessage::SendHave { piece_index })
            .await;

        // Write piece to disk
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

        if self
            .piece_mananger
            .as_ref()
            .expect("initalized")
            .have_all_pieces()
        {
            self.state = TorrentState::Seeding;
            if let Some(name) = self.metadata.display_name() {
                info!("Torrent-{} Download Completed", name);
            } else {
                info!("Torrent Download Completed");
            }
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

            self.broadcast_to_peers(PeerMessage::HaveMetadata(info))
                .await;
        }

        Ok(())
    }

    async fn clean_up_peer(&mut self, pid: Pid, bitfield: Option<Bitfield>) {
        // Notify choker that peer disconnected
        // Returns the peer that got unchoked to fill the slot (if any)
        if let Some(unchoked_pid) = self.choker.on_peer_disconnected(pid) {
            self.send_to_peer(unchoked_pid, PeerMessage::SendUnchoke)
                .await;
            tracing::debug!(
                "Peer disconnect: unchoked peer {:?} to fill slot",
                unchoked_pid
            );
        }

        let p = self.peers.remove(&pid);

        let requests: Vec<_> = p
            .unwrap()
            .pending_requests
            .iter()
            .map(|x| BlockRequest::from(*x))
            .collect();

        if let Some(bitfield) = bitfield
            && let Some(mananger) = self.piece_mananger.as_mut()
        {
            mananger.decrement_availability(&AvailabilityUpdate::Bitfield(&bitfield));
            mananger.cancel_peer_requests(&requests);
        }

        // Note: We do NOT decrement peers_discovered here because it tracks
        // cumulative discovered peers, not currently connected peers.
        // peers_connected (self.peers.len()) tracks current connections.
    }

    /// Run the choker algorithm periodically to rotate upload slots
    async fn run_choker(&mut self) {
        let (to_choke, to_unchoke) = self.choker.re_evaluate_unchokes();

        // Apply choke decisions
        for pid in to_choke {
            self.send_to_peer(pid, PeerMessage::SendChoke).await;
            tracing::debug!("Periodic choker: choked peer {pid:?}");
        }

        // Apply unchoke decisions
        for pid in to_unchoke {
            self.send_to_peer(pid, PeerMessage::SendUnchoke).await;
            tracing::debug!("Periodic choker: unchoked peer {pid:?}");
        }
    }

    /// Send a message to a specific peer
    async fn send_to_peer(&self, pid: Pid, message: PeerMessage) {
        if let Some(p) = self.peers.get(&pid) {
            let _ = p.tx.send_timeout(message, Duration::from_millis(50)).await;
        }
    }

    /// Send a message to a specific peer
    async fn broadcast_to_peers(&self, message: PeerMessage) {
        for pid in self.peers.keys() {
            self.send_to_peer(*pid, message.clone()).await;
        }
    }

    /// Send a protocol message to a specific peer
    async fn send_message_to_peer(&self, pid: Pid, message: Message) {
        self.send_to_peer(pid, PeerMessage::SendMessage(message))
            .await;
    }

    // Announce torrent over Tracker and DHT
    // internally creates dedicated tasks in charge of periodic announces
    // it implements max peer control
    // TODO: Have a signal handler that on SIGHUP Forces announce
    fn announce(&self, discovered_peers_tx: &mpsc::Sender<Vec<SocketAddr>>) {
        let client_state = self.metadata.info().map_or_else(
            || ClientState::new(0, 0, 0, Events::Started),
            |info| {
                ClientState::new(
                    0,
                    info.piece_length
                        * i64::try_from(info.pieces.len())
                            .expect("incoming info length > i64::MAX"),
                    0,
                    Events::Started,
                )
            },
        );

        // Spawn tracker announce tasks
        for announce_url in &self.trackers {
            let tracker_client = self.tracker_client.clone();
            let info_hash = self.info_hash;
            let announce = announce_url.to_string();
            let discovered_peers_tx = discovered_peers_tx.clone();
            tokio::spawn(async move {
                loop {
                    let response = tracker_client
                        .announce(info_hash, announce.clone(), client_state)
                        .await;

                    if let Err(e) = response {
                        tracing::warn!("Failed to announce to tracker {}: {}", announce, e);
                        return;
                    }

                    let Ok(response) = response else {
                        tracing::warn!("Tracker failure");
                        break;
                    };

                    let sleep_duration = response.interval;
                    let _ = discovered_peers_tx.send(response.peers).await;

                    sleep(Duration::from_secs(
                        u64::try_from(sleep_duration)
                            .expect("tracker interval must be  non-negative"),
                    ))
                    .await;
                }
            });
        }

        // Spawn DHT discovery task (runs in parallel with tracker announces)
        if let Some(dht) = self.dht_client.clone() {
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
