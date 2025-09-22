#![allow(dead_code)]
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use bittorrent_common::{metainfo::TorrentInfo, types::InfoHash};
use bytes::Bytes;
use magnet_uri::Magnet;
use peer_protocol::protocol::{BlockInfo, Message};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot, watch},
    time::{Instant, sleep},
};
use tracing::{debug, field::debug, info};
use tracker_client::{ClientState, Events, TrackerError, TrackerHandler, TrackerResponse};
use url::Url;

use crate::{
    Storage,
    bitfield::Bitfield,
    metadata::{Metadata, MetadataPiece, MetadataState},
    peer::peer_connection::{ConnectionError, spawn_outgoing_peer},
};

// Peer related
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Pid(usize);
static PEER_COUNTER: AtomicUsize = AtomicUsize::new(0);

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
    pub fn get_addr(&self) -> &SocketAddr {
        match self {
            Self::Inbound {
                stream: _,
                remote_addr,
                supports_ext: _,
            } => remote_addr,
            Self::Outbound(remote_addr) => remote_addr,
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
    PeerDisconnected(Pid),
    PeerError(Pid, ConnectionError),
    Have {
        pid: Pid,
        piece_idx: u32,
    },
    Bitfield(Pid, Vec<u8>),
    BlockRequest(Pid, BlockInfo),
    AddBlock(Pid, peer_protocol::protocol::Block),
    Interest(Pid),
    NotInterest(Pid),
    NeedTask(Pid),
    ValidMetadata {
        resp: oneshot::Sender<bool>,
    },

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

// Peer related

pub enum PeerMessage {
    RequestMetadata,
    SendHave { piece_index: u32 },
    SendBitfield { bitfield: Vec<u8> },
    SendChoke,
    SendUnchoke,
    Disconnect,
    SendMessage(Message),
}

pub(crate) struct PeerState {
    pub(crate) addr: SocketAddr,
    // pub(crate) bitfield: Bitfield,
    pub(crate) tx: mpsc::Sender<PeerMessage>,
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
    storage: Arc<Storage>,

    metrics: Arc<Metrics>,
    peers: HashMap<Pid, PeerState>,

    // channels
    torrent_tx: mpsc::Sender<TorrentMessage>,
    torrent_rx: mpsc::Receiver<TorrentMessage>,

    /// Shutdown signal
    shutdown_rx: watch::Receiver<()>,
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
        storage: Arc<Storage>,
        shutdown_rx: watch::Receiver<()>,
    ) -> (Self, mpsc::Sender<TorrentMessage>) {
        let info_hash = torrent_info.info_hash;
        let trackers = torrent_info
            .all_trackers()
            .into_iter()
            .filter_map(|url| Url::parse(&url).ok())
            .collect();

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
                storage,
                metrics: Arc::new(Metrics::default()),
                peers: HashMap::default(),
                shutdown_rx,
                torrent_tx: tx.clone(),
                torrent_rx: rx,
            },
            tx,
        )
    }

    /// Create a new Torrent from a magnet URI (metadata needs to be fetched)
    pub fn from_magnet(
        magnet: Magnet,
        tracker_client: Arc<TrackerHandler>,
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

        let (tx, rx) = mpsc::channel(64);

        (
            Self {
                info_hash,
                metadata,
                state: TorrentState::Leeching,
                trackers,
                tracker_client,
                storage,
                metrics: Arc::new(Metrics::default()),
                shutdown_rx,
                peers: HashMap::default(),
                torrent_tx: tx.clone(),
                torrent_rx: rx,
            },
            tx,
        )
    }

    /// Start torrent session
    pub async fn start_session(mut self) -> Result<(), TorrentError> {
        if self.metadata.has_metadata()
            && let Metadata::TorrentFile(file) = &self.metadata
        {
            self.storage.add_torrent(file.clone());
        }

        // Star announcing to Trackers/DHT
        let (announce_tx, mut announce_rx) = mpsc::channel(16);
        self.announce(announce_tx);

        loop {
            tokio::select! {
                Ok(_) = self.shutdown_rx.changed() => {
                    tracing::info!("Shutting down...");
                    break;
                }
                maybe_msg = self.torrent_rx.recv() => {
                    match maybe_msg{
                        Some(msg) => self.handle_message(msg).await?,
                        None => break,
                    }
                }
                Some(announce_msg) = announce_rx.recv() => {
                    for peer in announce_msg.peers.iter() {
                        self.add_peer(PeerSource::Outbound(*peer));
                    }

                }
            }
        }

        Ok(())
    }

    pub fn add_peer(&mut self, peer: PeerSource) {
        if self.peers.len() >= 15 {
            return;
        }
        // Create peer info
        let peer_id = PEER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let peer_id = Pid(peer_id);

        let (peer_tx, peer_rx) = mpsc::channel(64);

        info!("connecting to {}", peer.get_addr());
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
                self.torrent_tx.clone(),
                peer_rx,
            ),
        };

        let peer = PeerState {
            addr: *peer.get_addr(),
            tx: peer_tx,
        };

        self.peers.insert(peer_id, peer);
    }

    async fn handle_message(&mut self, msg: TorrentMessage) -> Result<(), TorrentError> {
        use TorrentMessage::*;
        match msg {
            PeerDisconnected(pid) => self.clean_up_peer(pid),
            PeerError(pid, err) => {
                // tracing::warn!(?err, "Peer {pid:?} encountered an error");
                // self.clean_up_peer(pid);
            }
            Have { pid, piece_idx } => {
                // Update our knowledge about what pieces the peer has
                tracing::debug!("Peer {pid:?} has piece {piece_idx}");
                // TODO: Update piece picker/availability map
            }
            Bitfield(pid, bitfield) => {
                tracing::debug!("Received bitfield from peer {pid:?}");
                // TODO: Update piece picker/availability map with full bitfield
            }
            BlockRequest(pid, block_info) => {
                // Handle a request for a block from a peer
                // if let Some(peer_tx) = self.peers.get(&pid) {
                //     TODO: Check if we have the block and can send it
                //     // If we have the block:
                //     if self.storage.has_block(&block_info) {
                //         let block_data = self.storage.read_block(&block_info).await.unwrap();
                //         let message = PeerProtocolMessage::Piece(peer_protocol::protocol::Block {
                //             index: block_info.index,
                //             begin: block_info.begin,
                //             data: block_data,
                //         });
                //         let _ = peer_tx.send(PeerMessage::SendMessage(message)).await;
                //     } else {
                //         // Reject if we don't have it
                //         let _ = peer_tx.send(PeerMessage::RejectRequest(block_info)).await;
                //     }
                // }
            }
            AddBlock(pid, block) => {
                // A peer has sent us a block
                tracing::debug!(
                    "Received block from peer {pid:?}: piece {}, offset {}",
                    block.index,
                    block.begin
                );

                // Write the block to storage
                // TODO: Handle the result properly
                // let _ = self.storage.write_block(block).await;

                // TODO: Update piece completion tracking
            }
            Interest(pid) => {
                tracing::debug!("Peer {pid:?} is interested in our pieces");
                // TODO: Consider unchoking this peer
            }
            NotInterest(pid) => {
                tracing::debug!("Peer {pid:?} is no longer interested in our pieces");
                // TODO: Consider choking this peer to save resources
            }
            NeedTask(pid) => {
                // Peer needs more blocks to download
                // if let Some(peer_tx) = self.peers.get(&pid) {
                //     // TODO: Get tasks from piece picker based on what the peer has
                //     let tasks = self.get_download_tasks_for_peer(pid);
                //     if !tasks.is_empty() {
                //         let _ = peer_tx.send(PeerMessage::AvailableTask(tasks)).await;
                //     }
                // }
            }
            ValidMetadata { resp } => {
                let _ = resp.send(self.metadata.has_metadata());
                debug_assert!(!self.metadata.has_metadata());
            }
            PeerWithMetadata { pid, metadata_size } => {
                tracing::debug!("recv metadata size");
                if let Metadata::MagnetUri { metadata_state, .. } = &mut self.metadata {
                    match metadata_state {
                        MetadataState::Pending => {
                            let total_pieces = metadata_size.div_ceil(1 << 14);
                            tracing::debug!(total_pieces);

                            *metadata_state = MetadataState::Fetching {
                                pieces_received: 0,
                                total_pieces,
                                metadata_size,
                                buf: bytes::BytesMut::with_capacity(metadata_size),
                                metadata_pieces: (0..total_pieces)
                                    .map(|_| MetadataPiece {
                                        num_req: 0,
                                        time_metadata_request: Instant::now(),
                                        have: false,
                                    })
                                    .collect(),
                            };
                        }
                        MetadataState::Fetching { .. } => {} // Already fetching
                        MetadataState::Complete(_) => return Ok(()), // Already complete
                    }
                }
            }
            FillMetadataRequest {
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
            ReceiveMetadata {
                pid,
                metadata,
                piece_idx,
            } => {
                if let Metadata::MagnetUri {
                    magnet,
                    metadata_state,
                } = &mut self.metadata
                    && let MetadataState::Fetching {
                        pieces_received,
                        total_pieces,
                        metadata_size,
                        buf,
                        metadata_pieces,
                    } = metadata_state
                {
                    self.metadata
                        .put_metadata_piece(metadata, piece_idx)
                        .unwrap();
                    // if Ok(())
                }
            }
        }
        Ok(())
    }

    fn clean_up_peer(&mut self, pid: Pid) {}

    /// Send a message to a specific peer
    async fn send_to_peer(&self, pid: Pid, message: PeerMessage) {
        if let Some(p) = self.peers.get(&pid) {
            let _ = p.tx.send(message).await;
        }
    }

    /// Send a protocol message to a specific peer
    async fn send_message_to_peer(&self, pid: Pid, message: Message) {
        self.send_to_peer(pid, PeerMessage::SendMessage(message))
            .await
    }

    // Announce torrent over Tracker
    // internally creates a dedicated task in charge of periodic announces
    // it implements max peer control
    fn announce(&self, announce_tx: mpsc::Sender<TrackerResponse>) {
        let client_state = if let Some(info) = self.metadata.info() {
            ClientState::new(
                0,
                info.piece_length * info.pieces.len() as i64,
                0,
                Events::Started,
            )
        } else {
            ClientState::new(0, 0, 0, Events::Started)
        };

        for announce_url in self.trackers.iter() {
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
                        return;
                    }

                    let response = response.unwrap();
                    let sleep_duration = response.interval;
                    let _ = announce_tx.send(response).await;

                    sleep(Duration::from_secs(sleep_duration as u64)).await
                }
            });
        }
    }

    fn process_announce(&self) {}
}
