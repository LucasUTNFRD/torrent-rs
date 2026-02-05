#![allow(dead_code)]

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
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
    metadata::{Metadata, MetadataState},
    peer::{
        PeerMessage, PeerState,
        metrics::PeerMetrics,
        peer_connection::{ConnectionError, spawn_outgoing_peer},
        retry_queue::PeerRetryQueue,
    },
    piece_picker::{AvailabilityUpdate, BlockRequest, PieceManager, PieceState},
};

// Peer related
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Pid(usize);

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}
static PEER_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Maximum number of connected peers allowed at one time
const MAX_CONNECTED_PEERS: usize = 50;

pub enum PeerSource {
    Inbound {
        stream: TcpStream,
        remote_addr: SocketAddr,
        supports_ext: bool,
    },
    Outbound(SocketAddr), // Socket to connect + Our peer id
}
//
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
}

//
// Metrics Control Structure
//

#[derive(Debug, Default)]
pub struct Metrics {
    pub downloaded_bytes: AtomicU64,
    pub uploaded_bytes: AtomicU64,
    pub connected_peers: AtomicUsize,
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
    retry_queue: PeerRetryQueue,
    retry_timer: tokio::time::Interval,

    // channels
    tx: mpsc::Sender<TorrentMessage>,
    rx: mpsc::Receiver<TorrentMessage>,

    // Download related
    piece_mananger: Option<PieceManager>,
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
        // let metadata = Arc::new(metadata);

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
                retry_queue: PeerRetryQueue::default(),
                retry_timer: tokio::time::interval(Duration::from_secs(30)),
                shutdown_rx,
                tx: tx.clone(),
                rx,
                bitfield,
                piece_mananger: None,
                // piece_picker: None,
                // piece_collector: None,
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

        // let metadata = Arc::new(metadata);

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
                retry_queue: PeerRetryQueue::default(),
                retry_timer: tokio::time::interval(Duration::from_secs(30)),
                tx: tx.clone(),
                rx,
                bitfield: Bitfield::new(),
                piece_mananger: None,
                // piece_collector: None,
            },
            tx,
        )
    }

    #[instrument(skip(self), name = "torrent", fields(
    info_hash=%self.info_hash,
    ))]
    pub async fn start_session(mut self) -> Result<(), TorrentError> {
        self.init_interal();

        // Star announcing to Trackers/DHT
        let (announce_tx, mut announce_rx) = mpsc::channel(16);
        self.announce(&announce_tx);

        // let mut debug_interval = tokio::time::interval(Duration::from_secs(60));

        loop {
            if let Some(piece_manager) = self.piece_mananger.as_ref()
                && piece_manager.have_all_pieces()
            {
                if let Some(name) = self.metadata.display_name() {
                    info!("Torrent-{} Download Completed", name);
                } else {
                    info!("Torrent Download Completed");
                }
                //  TODO: BEFORFE BREAKING WE SHOULD DO SOME MERCIFUL SHUTDOWN OF PEER CONNECTIONS
                break;
            }
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
                Some(announce_msg) = announce_rx.recv() => {
                    info!("received peers: {:?}",announce_msg.peers);
                    for peer in &announce_msg.peers {
                        self.add_peer(PeerSource::Outbound(*peer));
                    }

                }
                _ = self.retry_timer.tick() => {
                    self.process_retry_queue();
                }
                // _ = debug_interval.tick() => {
                //       if let Some(ref piece_manager) = self.piece_mananger {
                //         piece_manager.debug_print_state();
                //     }
                // }
            }
        }

        Ok(())
    }

    // init bitfield
    // init piece picker
    // init piece collector
    fn init_interal(&mut self) {
        if !self.metadata.has_metadata() {
            debug("Should return here");
            return;
        }

        match &self.metadata {
            Metadata::TorrentFile(torrent_info) => {
                self.storage
                    .add_torrent(self.info_hash, torrent_info.info.clone());

                let info = torrent_info.info.clone();
                self.bitfield = Bitfield::with_size(info.pieces.len());
                self.piece_mananger = Some(PieceManager::new(info));
            }

            Metadata::MagnetUri {
                metadata_state: MetadataState::Complete(info),
                ..
            } => {
                self.storage.add_torrent(self.info_hash, info.clone());
                self.bitfield = Bitfield::with_size(info.pieces.len());

                self.piece_mananger = Some(PieceManager::new(info.clone()));
            }
            Metadata::MagnetUri { .. } => {
                panic!("called this in wrong state")
            }
        }
    }

    pub fn add_peer(&mut self, peer: PeerSource) {
        let peer_addr = *peer.get_addr();

        // check if we already are connected to this peer?
        let already_connected = self.peers.values().any(|p| p.addr == peer_addr);
        if already_connected {
            return;
        }

        if self.peers.len() >= MAX_CONNECTED_PEERS {
            // Add to retry queue to try again later when peer slots are available
            self.retry_queue.add_pending_peer(peer_addr, Duration::from_secs(30));
            info!("MAX PEERS REACHED, queued {} for later retry", peer_addr);
            return;
        }

        // Remove from retry queue if present (we're trying to connect now)
        if self.retry_queue.contains(&peer_addr) {
            self.retry_queue.remove_peer(&peer_addr);
        }

        // Create peer info
        let peer_id = PEER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let peer_id = Pid(peer_id);

        let (peer_tx, peer_rx) = mpsc::channel(64);

        info!("connecting to {}", peer.get_addr());

        self.metrics.connected_peers.fetch_add(1, Ordering::Relaxed);
        match peer {
            PeerSource::Inbound {
                stream,
                remote_addr,
                supports_ext,
            } => todo!(),
            PeerSource::Outbound(remote_addr) => spawn_outgoing_peer(
                peer_id,
                remote_addr,
                self.info_hash,
                self.tx.clone(),
                peer_rx,
            ),
        }

        let peer = PeerState {
            addr: *peer.get_addr(),
            tx: peer_tx,
            metrics: PeerMetrics::new(),
            pending_requests: Vec::new(),
        };

        self.peers.insert(peer_id, peer);
    }

    async fn handle_message(&mut self, msg: TorrentMessage) -> Result<(), TorrentError> {
        match msg {
            TorrentMessage::PeerDisconnected(pid, bitfield) => {
                self.clean_up_peer(pid, bitfield, None)
            }
            TorrentMessage::PeerError(pid, err, bitfield) => {
                self.clean_up_peer(pid, bitfield, Some(err));
            }
            TorrentMessage::Have { pid, piece_idx } => {
                // let p = self.piece_picker.as_mut().expect("init");
                // p.increment_availability(AvailabilityUpdate::Have(piece_idx));
            }
            TorrentMessage::ReceiveBlock(pid, block) => {
                tracing::debug!("Incoming block {block:?}");
                self.incoming_block(pid, block).await;
            }
            TorrentMessage::Interest(pid) => {
                tracing::debug!("Peer {pid:?} is interested in our pieces");
                // TODO: Consider unchoking this peer
            }
            TorrentMessage::NotInterest(pid) => {
                tracing::debug!("Peer {pid:?} is no longer interested in our pieces");
                // TODO: Consider choking this peer to save resources
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
                    tracing::warn!("Failed to set metadata size: {}", e);
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
                    tracing::warn!("Failed to put metadata piece {}: {}", piece_idx, e);
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
                    tokio::task::spawn(async move { dht_client.try_add_node(node_addr).await });
                }
            }
        }

        Ok(())
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

        // TODO: IF OTHER PEER IS ALSO REQUESTING THIS PIECE WE SHOULD SEND CANCEL
        //  AND REMOVEIT from its pending requests

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
        let p = self.piece_mananger.as_mut().expect("inti");
        p.set_piece_as(piece_index as usize, PieceState::Downloaded);

        let torrent_id = self.info_hash;
        let piece: Arc<[u8]> = piece.into();

        let (verification_tx, verification_rx) = oneshot::channel();
        // let t = Instant::now(); // ANALYZE TIME SPENT VALIDATING
        self.storage
            .verify_piece(torrent_id, piece_index, piece.clone(), verification_tx);

        let valid = verification_rx.await.expect("storage actor alive");

        // let t = t.elapsed();
        // tracing::info!("PIECE VALIDATION RECV- TOOK {t:?}");

        if !valid {
            p.set_piece_as(piece_index as usize, PieceState::NotRequested);
            return;
        }

        p.set_piece_as(piece_index as usize, PieceState::Have);
        self.bitfield.set(piece_index as usize);
        // tracing::debug!("BROADCASTING PIECE TO PEERS");
        self.broadcast_to_peers(PeerMessage::SendHave { piece_index })
            .await;

        self.piece_mananger
            .as_ref()
            .expect("initalized")
            .info_progress();

        self.storage.write_piece(torrent_id, piece_index, piece);
    }

    // 1.Register torrent in storage
    // 2.Build bitfield for this torrent
    // 3.Notify peer connection control structure about have metainfo for ending ut_metadfata
    // fetching
    async fn on_complete_metadata(&mut self) -> Result<(), TorrentError> {
        if let Err(e) = self.metadata.construct_info() {
            tracing::error!("Failed to construct info from metadata: {}", e);
        } else {
            self.init_interal();

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

    fn clean_up_peer(
        &mut self,
        pid: Pid,
        bitfield: Option<Bitfield>,
        error: Option<ConnectionError>,
    ) {
        info!("peer disconnected {pid:?}");

        let peer_addr = self.peers.get(&pid).map(|p| p.addr);

        let p = self.peers.remove(&pid);

        if let Some(addr) = peer_addr {
            // Add to retry queue if there was an error
            if error.is_some() {
                self.retry_queue.add_failed_peer(addr, error.as_ref());
                info!("Added peer {} to retry queue", addr);
            }
        }

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

        self.metrics.connected_peers.fetch_sub(1, Ordering::Relaxed);
    }

    /// Process the retry queue and attempt to reconnect to ready peers
    fn process_retry_queue(&mut self) {
        let available_slots = MAX_CONNECTED_PEERS.saturating_sub(self.peers.len());
        if available_slots == 0 {
            return;
        }

        let now = Instant::now();
        let ready_peers = self.retry_queue.get_ready_peers(now);

        for addr in ready_peers.into_iter().take(available_slots) {
            info!("Retrying connection to peer {}", addr);
            self.add_peer(PeerSource::Outbound(addr));
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
    fn announce(&self, announce_tx: &mpsc::Sender<TrackerResponse>) {
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
            let announce_tx = announce_tx.clone();
            tokio::spawn(async move {
                loop {
                    let response = tracker_client
                        .announce(info_hash, announce.clone(), client_state)
                        .await;

                    if let Err(e) = response {
                        tracing::warn!("Failed to announce to tracker {}: {}", announce, e);
                        // TODO: Implement retry mechanism
                        return;
                    }

                    let Ok(response) = response else {
                        tracing::warn!("Tracker failure");
                        break;
                    };

                    let sleep_duration = response.interval;
                    let _ = announce_tx.send(response).await;

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
            let announce_tx = announce_tx.clone();
            let port = 6881_u16; // TODO: Use actual listening port from session

            tokio::spawn(async move {
                // DHT re-announce interval (15 minutes as per BEP 5 recommendation)
                const DHT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(15 * 60);

                loop {
                    // Perform DHT announce (get_peers + announce_peer)
                    match dht.announce(info_hash, port).await {
                        Ok(response) => {
                            let peer_count = response.peers.len();
                            if peer_count > 0 {
                                tracing::info!("DHT: found {} peers for {}", peer_count, info_hash);

                                // Convert DhtResponse to TrackerResponse for unified handling
                                let tracker_response = TrackerResponse {
                                    peers: response.peers,
                                    interval: i32::try_from(DHT_ANNOUNCE_INTERVAL.as_secs())
                                        .expect("DHT_ANNOUNCE_INTERVAL exceeds i32 range"),
                                    leechers: 0,
                                    seeders: 0,
                                };
                                let _ = announce_tx.send(tracker_response).await;
                            } else {
                                tracing::debug!(
                                    "DHT: no peers found for {}, will retry",
                                    info_hash
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("DHT announce failed for {}: {}", info_hash, e);
                        }
                    }

                    // Wait before next DHT announce
                    sleep(DHT_ANNOUNCE_INTERVAL).await;
                }
            });
        }
    }
}
