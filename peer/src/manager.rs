use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use bitfield::Bitfield;
use bittorrent_core::{metainfo::TorrentInfo, types::PeerID};
use bytes::Bytes;
use peer_protocol::protocol::Message;
use tokio::sync::mpsc;

use crate::{
    PeerError,
    connection::{PeerInfo, spawn_peer},
};

#[derive(Debug)]
pub enum ManagerCommand {
    AddPeer {
        peer_addr: SocketAddr,
        our_client_id: PeerID,
    },
    RemovePeer(SocketAddr),
}

#[derive(Debug)]
pub enum PeerEvent {
    Have { pid: Id, piece_idx: u32 },
    Bitfield(Id, Bytes),
    PeerError(Id, PeerError),
}

#[derive(Debug)]
pub enum PeerCommand {
    SendMessage(Message),
    Disconnect,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Id(usize);
static PEER_COUNTER: AtomicUsize = AtomicUsize::new(0);

// Peer manager is in charge of choking and unchoking

struct PeerConnectionConfig {}

//  A handle is an object that other pieces of code can use to talk to the actor, and is also what keeps the actor alive.
#[derive(Debug, Clone)]
pub struct PeerManagerHandle {
    manager_tx: mpsc::UnboundedSender<ManagerCommand>,
}

impl PeerManagerHandle {
    // TODO: Implement a manager builder to pass a config
    pub fn new(torrent: Arc<TorrentInfo>) -> Self {
        let (manager_tx, manager_rx) = mpsc::unbounded_channel();

        let manager = PeerManager::new(manager_rx, torrent);
        tokio::spawn(async move { manager.run().await });

        Self { manager_tx }
    }

    pub fn add_peer(&self, addr: SocketAddr, client_id: PeerID) -> Result<(), PeerError> {
        self.manager_tx
            .send(ManagerCommand::AddPeer {
                peer_addr: addr,
                our_client_id: client_id,
            })
            .map_err(|_| PeerError::Disconnected)?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), PeerError> {
        todo!()
    }
}

impl PeerManagerHandle {}

struct PeerManager {
    torrent: Arc<TorrentInfo>,
    peers: HashMap<Id, PeerState>,
    manager_rx: mpsc::UnboundedReceiver<ManagerCommand>,
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_event_rx: mpsc::Receiver<PeerEvent>,

    // Download state
    bitfield: Bitfield,
    // Actor managers
    //disk
    //picker
    //choker
}

#[derive(Debug, Clone)]
struct PeerState {
    pub pid: Id,
    pub sender: mpsc::Sender<PeerCommand>,
    pub bitfield: Bitfield,
}

impl PeerManager {
    pub fn new(
        manager_rx: mpsc::UnboundedReceiver<ManagerCommand>,
        torrent: Arc<TorrentInfo>,
    ) -> Self {
        let (peer_event_tx, peer_event_rx) = mpsc::channel(64);
        Self {
            peers: HashMap::new(),
            manager_rx,
            peer_event_tx,
            peer_event_rx,
            bitfield: Bitfield::new(torrent.num_pieces()),
            torrent,
        }
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                maybe_cmd= self.manager_rx.recv()=>{
                    match maybe_cmd {
                        Some(cmd) => self.handle_cmd(cmd).await,
                        None => break, // FIX: Gracefully shutdown peers
                    }
                },
                maybe_peer_cmd= self.peer_event_rx.recv()=>{
                    match maybe_peer_cmd {
                        Some(event) => self.handle_peer_event(event).await,
                        None => tracing::warn!("peer_event channel closed"),
                    }
                },
            }
        }
    }

    async fn handle_cmd(&mut self, cmd: ManagerCommand) {
        use ManagerCommand::*;
        match cmd {
            AddPeer {
                peer_addr,
                our_client_id,
            } => {
                let id = PEER_COUNTER.fetch_add(1, Ordering::Relaxed);
                let id = Id(id);
                let peer_info = PeerInfo::new(our_client_id, id, self.torrent.info_hash, peer_addr);
                let peer_tx = spawn_peer(peer_info, self.peer_event_tx.clone());
                self.peers.insert(
                    id,
                    PeerState {
                        pid: id,
                        sender: peer_tx,
                        bitfield: Bitfield::new(self.torrent.num_pieces()),
                    },
                );
            }
            _ => unimplemented!(),
        }
    }
    async fn handle_peer_event(&mut self, cmd: PeerEvent) {
        use PeerEvent::*;
        match cmd {
            Have { pid, piece_idx } => {
                let peer = match self.peers.get_mut(&pid) {
                    Some(peer) => peer,
                    None => return,
                };
                if let Err(e) = peer.bitfield.set(piece_idx as usize) {
                    tracing::warn!("set operation failed {e}");
                }
            }
            Bitfield(pid, payload) => {
                // A bitfield of the wrong length is considered an error.
                // Clients should drop the connection if they receive bitfields that are not of the correct size, or if the bitfield has any of the spare bits set.
                let peer = match self.peers.get_mut(&pid) {
                    Some(peer) => peer,
                    None => return,
                };

                match bitfield::Bitfield::try_from((payload, self.torrent.num_pieces())) {
                    Ok(bitfield) => peer.bitfield = bitfield,
                    Err(e) => {
                        let peer = self.peers.remove(&pid).unwrap();
                        let _ = peer.sender.send(PeerCommand::Disconnect).await;
                    }
                };
            }
            PeerError(id, err) => {}
        }
    }
}

mod picker {
    use std::sync::Arc;

    use bittorrent_core::metainfo::TorrentInfo;

    // reference: https://blog.libtorrent.org/2011/11/writing-a-fast-piece-picker/
    struct Picker {
        torrent: Arc<TorrentInfo>,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub enum PieceState {
        None,
        Requested,
        Writing,
        Finished,
    }

    impl Picker {
        pub fn new(torrent: Arc<TorrentInfo>) -> Self {
            Self { torrent }
        }

        pub fn pick_piece() {}
    }
}

mod bitfield {
    use bytes::Bytes;
    use thiserror::Error;

    // Fixed size → so we don’t need Vec, just a boxed slice.
    // Mutation only by a manager → internal mutability isn’t needed if you keep it behind &mut or an owner.
    // Shared as snapshot → when handed out, others shouldn’t observe future mutations.
    #[derive(Debug, Clone, Eq, PartialEq)]
    pub struct Bitfield {
        bits: Box<[u8]>,
        nbits: usize,
    }

    #[derive(Debug, Error, PartialEq, Eq)]
    pub enum BitfieldError {
        #[error("Invalid Length expected{expected_len}, got {actual_len}")]
        InvalidLength {
            expected_len: usize,
            actual_len: usize,
        },
        #[error("Non zero spare bits")]
        NonZeroSpareBits,
        #[error("Index {idx} out of bounds (len {len})")]
        OutOfBounds { idx: usize, len: usize },
    }

    impl TryFrom<(Bytes, usize)> for Bitfield {
        type Error = BitfieldError;

        fn try_from((bytes, num_pieces): (Bytes, usize)) -> Result<Self, Self::Error> {
            let expected_bytes = (num_pieces + 7) / 8;

            if bytes.len() < expected_bytes {
                return Err(BitfieldError::InvalidLength {
                    expected_len: expected_bytes,
                    actual_len: bytes.len(),
                });
            }

            // Check spare bits in the last byte
            let last_byte_bits = num_pieces % 8;
            if last_byte_bits != 0 {
                // If num_pieces is not a multiple of 8
                let last_byte = bytes[expected_bytes - 1];
                let mask = (1u8 << (8 - last_byte_bits)) - 1; // Mask for spare bits
                if (last_byte & mask) != 0 {
                    return Err(BitfieldError::NonZeroSpareBits);
                }
            }

            // Check trailing bytes
            if bytes.len() > expected_bytes {
                let extra_bytes = &bytes[expected_bytes..];
                if extra_bytes.iter().any(|&b| b != 0) {
                    return Err(BitfieldError::NonZeroSpareBits);
                }
            }

            Ok(Self {
                bits: Box::from(&bytes[..]),
                nbits: num_pieces,
            })
        }
    }

    impl Bitfield {
        pub fn new(nbits: usize) -> Self {
            let nbytes = (nbits + 7) / 8;
            Self {
                bits: vec![0; nbytes].into_boxed_slice(),
                nbits,
            }
        }

        pub fn as_bytes(&self) -> Bytes {
            Bytes::from(self.bits.clone())
        }

        pub fn get(&self, index: usize) -> bool {
            if index >= self.nbits {
                return false;
            }
            let byte_index = index / 8;
            let bit_index = 7 - (index % 8);
            (self.bits[byte_index] >> bit_index) & 1 != 0
        }

        pub fn set(&mut self, index: usize) -> Result<(), BitfieldError> {
            if index >= self.nbits {
                return Err(BitfieldError::OutOfBounds {
                    idx: index,
                    len: self.nbits,
                });
            }
            let byte_index = index / 8;
            let bit_index = 7 - (index % 8);
            self.bits[byte_index] |= 1 << bit_index;
            Ok(())
        }
    }
}
