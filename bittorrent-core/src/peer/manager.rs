use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::piece_picker::{AvailabilityUpdate, Picker, PieceState};
use bitfield::Bitfield;
use bittorrent_common::{metainfo::TorrentInfo, types::PeerID};
use bytes::Bytes;
use peer_protocol::protocol::{Block, BlockInfo, Message};
use piece_cache::PieceCache;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use crate::{
    peer::connection::{PeerInfo, spawn_peer},
    storage::Storage,
};

use super::error::PeerError;

#[derive(Debug)]
pub enum ManagerCommand {
    AddPeer {
        peer_addr: SocketAddr,
        our_client_id: PeerID,
    },
    // RemovePeer(SocketAddr),
    Shutdown,
}

#[derive(Debug)]
pub enum PeerEvent {
    Have { pid: Id, piece_idx: u32 },
    Bitfield(Id, Bytes),
    PeerError(Id, PeerError),
    AddBlock(Id, Block),
    NeedTask(Id),
}

#[derive(Debug)]
pub enum PeerCommand {
    SendMessage(Message),
    Disconnect,
    AvailableTask(Vec<BlockInfo>),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Id(pub usize);
static PEER_COUNTER: AtomicUsize = AtomicUsize::new(0);

//  A handle is an object that other pieces of code can use to talk to the actor, and is also what keeps the actor alive.
#[derive(Debug)]
#[allow(dead_code)]
pub struct PeerManagerHandle {
    manager_tx: mpsc::UnboundedSender<ManagerCommand>,
    pub handle: JoinHandle<()>,
}

impl PeerManagerHandle {
    // TODO: Implement a manager builder to pass a config
    pub fn new(torrent: Arc<TorrentInfo>, storage: Arc<Storage>) -> Self {
        let (manager_tx, manager_rx) = mpsc::unbounded_channel();

        let manager = PeerManager::new(manager_rx, torrent, storage);
        let handle = tokio::spawn(async move { manager.run().await });

        Self { manager_tx, handle }
    }

    pub fn add_peer(&self, addr: SocketAddr, client_id: PeerID) {
        let _ = self.manager_tx.send(ManagerCommand::AddPeer {
            peer_addr: addr,
            our_client_id: client_id,
        });
    }

    pub fn shutdown(&self) {
        let _ = self.manager_tx.send(ManagerCommand::Shutdown);
    }
}

impl PeerManagerHandle {}

struct PeerManager {
    torrent: Arc<TorrentInfo>,
    peers: HashMap<Id, PeerState>,
    manager_rx: mpsc::UnboundedReceiver<ManagerCommand>,
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_event_rx: mpsc::Receiver<PeerEvent>,

    connected_addrs: HashSet<SocketAddr>,

    // Download state
    bitfield: Bitfield,
    // Actor managers
    //disk
    picker: Picker,
    cache: PieceCache,
    storage: Arc<Storage>,
    //choker
}

#[derive(Debug, Clone)]
struct PeerState {
    #[allow(dead_code)]
    pub pid: Id,
    pub addr: SocketAddr,
    pub sender: mpsc::Sender<PeerCommand>,
    pub bitfield: Bitfield,
    pub am_interested: bool,
    pub assigned_piece_idx: Option<usize>,
}

impl PeerManager {
    pub fn new(
        manager_rx: mpsc::UnboundedReceiver<ManagerCommand>,
        torrent: Arc<TorrentInfo>,
        storage: Arc<Storage>,
    ) -> Self {
        let (peer_event_tx, peer_event_rx) = mpsc::channel(64);
        Self {
            peers: HashMap::new(),
            manager_rx,
            peer_event_tx,
            peer_event_rx,
            cache: PieceCache::new(torrent.clone()),
            bitfield: Bitfield::new(torrent.num_pieces()),
            picker: Picker::new(torrent.clone()),
            torrent,
            storage,
            connected_addrs: HashSet::new(),
        }
    }

    pub async fn run(mut self) {
        loop {
            if self.bitfield.all_set() {
                tracing::info!(
                    "torrent file {} has been leeched succesfully",
                    self.torrent.info.mode.name()
                );

                let (none, requested, writing, finished) = self.picker.summary();
                dbg!(none, requested, writing, finished);
            }

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
                // TODO: Check that peer is not already connected
                if self.connected_addrs.contains(&peer_addr) {
                    tracing::debug!(?peer_addr, "we are alreay connected to this peer");
                    return;
                }

                self.connected_addrs.insert(peer_addr);

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
                        am_interested: false,
                        assigned_piece_idx: None,
                        addr: peer_addr,
                    },
                );
            }
            Shutdown => {
                for (_, peer_state) in self.peers.iter() {
                    let _ = peer_state.sender.send(PeerCommand::Disconnect).await;
                }
            }
        }
    }

    async fn handle_interest(&mut self, pid: Id) {
        let peer = match self.peers.get_mut(&pid) {
            Some(p) => p,
            None => return,
        };

        if peer.am_interested {
            return;
        }

        let should_send_interest = self
            .picker
            .send_interest(AvailabilityUpdate::Bitfield(&peer.bitfield));

        if should_send_interest {
            peer.am_interested = true;
            let _ = peer
                .sender
                .send(PeerCommand::SendMessage(Message::Interested))
                .await;

            self.assign_piece(pid).await;
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
                    return;
                }

                self.picker
                    .increment_availability(AvailabilityUpdate::Index(piece_idx));

                self.handle_interest(pid).await;
            }
            Bitfield(pid, payload) => {
                // A bitfield of the wrong length is considered an error.
                // Clients should drop the connection if they receive bitfields that are not of the correct size, or if the bitfield has any of the spare bits set.
                let peer = match self.peers.get_mut(&pid) {
                    Some(peer) => peer,
                    None => return,
                };

                match bitfield::Bitfield::try_from((payload, self.torrent.num_pieces())) {
                    Ok(bitfield) => {
                        tracing::debug!("setting bitfield to peer");
                        peer.bitfield = bitfield;

                        self.picker
                            .increment_availability(AvailabilityUpdate::Bitfield(&peer.bitfield));

                        // Send our bitfield if we have at least one piece
                        if !self.bitfield.is_empty() {
                            let _ = peer
                                .sender
                                .send(PeerCommand::SendMessage(Message::Bitfield(
                                    self.bitfield.as_bytes(),
                                )))
                                .await;
                        }

                        self.handle_interest(pid).await;
                    }
                    Err(e) => {
                        tracing::warn!("dropping peer connection - reason {e}");
                        let _ = peer.sender.send(PeerCommand::Disconnect).await;
                        self.clean_up_peer(pid);
                    }
                };
            }
            AddBlock(id, block) => {
                // cache the block
                if let Some(completed_piece) = self.cache.insert_block(&block) {
                    if let Some(peer) = self.peers.get_mut(&id) {
                        peer.assigned_piece_idx = None;
                    }

                    let piece_index = block.index;
                    let torrent_id = self.torrent.info_hash;

                    // Mark the piece as writing
                    self.picker
                        .mark_piece_as(piece_index as usize, PieceState::Writing);

                    // let start_timer = Instant::now();
                    let (verification_tx, verification_rx) = oneshot::channel();
                    self.storage.verify_piece(
                        torrent_id,
                        piece_index,
                        completed_piece.clone(),
                        verification_tx,
                    );

                    // WARN: Verifying a piece is blocking
                    let valid = verification_rx
                        .await
                        .expect("failed to receive piece validation");

                    if valid {
                        self.storage
                            .write_piece(torrent_id, piece_index, completed_piece.clone());
                        self.bitfield.set(piece_index as usize).unwrap();
                        self.picker
                            .mark_piece_as(piece_index as usize, PieceState::Finished);
                        self.broadcast_have(piece_index);
                    } else {
                        //  marking the piece as none results in downloading from zero the piece
                        tracing::debug!("piece {} was invalid", piece_index);
                        self.picker
                            .mark_piece_as(piece_index as usize, PieceState::None);
                        self.cache.drop_piece(piece_index as usize);
                    }
                    // let handshake_duration = start_timer.elapsed();
                    // tracing::debug!(duration = ?handshake_duration, "validation and writing completed");
                }
            }
            NeedTask(id) => {
                let peer_has_assignment = self
                    .peers
                    .get(&id)
                    .map(|p| p.assigned_piece_idx.is_some())
                    .unwrap_or(false);

                let (none, requested, writing, finished) = self.picker.summary();
                tracing::debug!(
                    peer = %id.0,
                    none = none,
                    requested = requested,
                    writing = writing,
                    finished = finished,
                    "no pieces available for peer"
                );

                if !peer_has_assignment {
                    self.assign_piece(id).await;
                }
            }
            PeerError(id, err) => {
                tracing::warn!(?err, ?id);
                self.clean_up_peer(id);
            }
        }
    }

    async fn assign_piece(&mut self, id: Id) {
        let peer = match self.peers.get_mut(&id) {
            Some(peer) => peer,
            None => return,
        };

        // Only assign when we are interested and the peer has no piece assigned
        if !peer.am_interested {
            tracing::debug!(?id, "skipping assignment: not interested");
            return;
        }

        if peer.assigned_piece_idx.is_some() {
            tracing::debug!(?id, "skipping assignment: already has an assigned piece");
            return;
        }

        if let Some((idx, piece)) = self.picker.pick_piece(&peer.bitfield) {
            // Verify piece is actually in None state
            peer.assigned_piece_idx = Some(idx);
            self.picker.mark_piece_as(idx, PieceState::Requested);
            let _ = peer.sender.send(PeerCommand::AvailableTask(piece)).await;
            tracing::debug!("Assigned piece {} to peer {}", idx, id.0);
        } else {
            let (none, requested, writing, finished) = self.picker.summary();
            tracing::debug!(
                peer = %id.0,
                none = none,
                requested = requested,
                writing = writing,
                finished = finished,
                "no pieces available for peer"
            );
        }
    }

    fn broadcast_have(&self, piece_index: u32) {
        for state in self.peers.values() {
            let sender = state.sender.clone();
            tokio::spawn(async move {
                let _ = sender
                    .send(PeerCommand::SendMessage(Message::Have { piece_index }))
                    .await;
            });
        }
    }

    //Removes disconnected peer from the peers HashMap
    // Decrements piece availability for all pieces the peer had
    fn clean_up_peer(&mut self, id: Id) {
        if let Some(removed_peer) = self.peers.remove(&id) {
            self.picker
                .decrement_availability(AvailabilityUpdate::Bitfield(&removed_peer.bitfield));

            self.connected_addrs.remove(&removed_peer.addr);

            if let Some(piece_idx) = removed_peer.assigned_piece_idx {
                self.picker.mark_piece_as(piece_idx, PieceState::None);
                self.cache.drop_piece(piece_idx);
            }
        }
    }
}

mod piece_cache {
    use std::{collections::HashMap, sync::Arc};

    use bittorrent_common::metainfo::TorrentInfo;
    use peer_protocol::protocol::Block;

    pub struct PieceCache {
        pieces: HashMap<usize, PieceMetadata>,
    }

    struct PieceMetadata {
        buffer: Box<[u8]>,
        piece_length: usize,
        downloaded: usize,
    }

    impl PieceCache {
        pub fn new(torrent: Arc<TorrentInfo>) -> Self {
            let mut pieces = HashMap::with_capacity(torrent.num_pieces());
            for i in 0..torrent.num_pieces() {
                let piece_length = torrent.get_piece_len(i) as usize;
                let buffer = vec![0; piece_length].into_boxed_slice();
                let piece_metadata = PieceMetadata {
                    buffer,
                    piece_length,
                    downloaded: 0,
                };
                pieces.insert(i, piece_metadata);
            }
            Self { pieces }
        }

        pub fn insert_block(&mut self, block: &Block) -> Option<Arc<[u8]>> {
            let index = block.index as usize;

            let piece = self.pieces.get_mut(&index)?;
            let offset = block.begin as usize;
            piece.buffer[offset..offset + block.data.len()].copy_from_slice(&block.data);
            // BUG: WE overcount if presence of duplicates
            piece.downloaded += block.data.len();
            if piece.downloaded >= piece.piece_length {
                // Remove the piece from the cache
                let piece = self.pieces.remove(&index).unwrap();
                return Some(piece.buffer.into());
            }
            None
        }

        pub fn drop_piece(&mut self, piece_idx: usize) {
            self.pieces.remove(&piece_idx);
        }
    }
}

pub mod bitfield {

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

        pub fn is_empty(&self) -> bool {
            self.bits.iter().all(|b| *b == 0)
        }

        pub fn all_set(&self) -> bool {
            if self.nbits == 0 {
                return false;
            }

            let full_words = self.nbits / 32;
            let remaining_bits = self.nbits % 32;

            // Check full 32-bit words
            for i in 0..full_words {
                let idx = i * 4;
                let word = u32::from_be_bytes(self.bits[idx..idx + 4].try_into().unwrap());
                if word != 0xFFFF_FFFF {
                    return false;
                }
            }

            // Check leftover bits in last partial word
            if remaining_bits > 0 {
                let idx = full_words * 4;
                let mut last_word_bytes = [0u8; 4];
                for (j, byte) in self.bits[idx..].iter().take(4).enumerate() {
                    last_word_bytes[j] = *byte;
                }
                let word = u32::from_be_bytes(last_word_bytes);
                let mask = 0xFFFF_FFFF_u32 << (32 - remaining_bits);
                if word & mask != mask {
                    return false;
                }
            }

            true
        }

        #[allow(dead_code)]
        pub fn has(&self, index: usize) -> bool {
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

        pub fn iter_set(&self) -> BitfieldSetIter<'_> {
            BitfieldSetIter {
                bitfield: self,
                byte_idx: 0,
                bit_in_byte: 0,
            }
        }
    }

    pub struct BitfieldSetIter<'a> {
        bitfield: &'a Bitfield,
        byte_idx: usize,
        bit_in_byte: u8,
    }

    impl Iterator for BitfieldSetIter<'_> {
        type Item = usize;

        fn next(&mut self) -> Option<Self::Item> {
            while self.byte_idx < self.bitfield.bits.len() {
                let byte = self.bitfield.bits[self.byte_idx];
                while self.bit_in_byte < 8 {
                    let mask = 1u8 << (7 - self.bit_in_byte);
                    let idx = self.byte_idx * 8 + self.bit_in_byte as usize;
                    self.bit_in_byte += 1;

                    // Skip spare bits beyond nbits
                    if idx >= self.bitfield.nbits {
                        continue;
                    }

                    if (byte & mask) != 0 {
                        return Some(idx);
                    }
                }
                self.byte_idx += 1;
                self.bit_in_byte = 0;
            }
            None
        }
        // /// Yields Index of piece marked in bitfield
        // fn next(&mut self) -> Option<Self::Item> {
        //     while self.word_idx < self.bitfield.bits.len() {
        //         let word = self.bitfield.bits[self.word_idx];
        //         while self.bit_mask != 0 {
        //             if word & self.bit_mask != 0 {
        //                 let idx = self.word_idx * 32 + self.bit_mask.leading_zeros() as usize;
        //                 self.bit_mask >>= 1;
        //                 if idx < self.bitfield.nbits {
        //                     return Some(idx);
        //                 }
        //             } else {
        //                 self.bit_mask >>= 1;
        //             }
        //         }
        //         self.word_idx += 1;
        //         self.bit_mask = 0x80;
        //     }
        //     None
        // }
    }

    #[cfg(test)]
    mod test {
        use super::*;

        #[test]
        fn test_all_set() {
            // Empty bitfield
            let bf = Bitfield::new(0);
            assert!(!bf.all_set());

            // Single bit, unset
            let mut bf = Bitfield::new(1);
            assert!(!bf.all_set());

            // Single bit, set
            bf.set(0).unwrap();
            assert!(bf.all_set());

            // Multiple bits, all set
            let mut bf = Bitfield::new(10);
            for i in 0..10 {
                bf.set(i).unwrap();
            }
            assert!(bf.all_set());

            // Multiple bits, one unset
            let mut bf = Bitfield::new(10);
            for i in 0..9 {
                bf.set(i).unwrap();
            }
            assert!(!bf.all_set());

            // Edge case: last partial byte
            let mut bf = Bitfield::new(14);
            for i in 0..14 {
                bf.set(i).unwrap();
            }
            assert!(bf.all_set());

            // Edge case: last partial byte, one bit unset
            let mut bf = Bitfield::new(14);
            for i in 0..13 {
                bf.set(i).unwrap();
            }
            assert!(!bf.all_set());
        }

        #[test]
        fn test_set_iter() {
            // Create a bitfield with 10 bits
            let mut bf = Bitfield::new(10);

            // No bits set yet
            let bits: Vec<usize> = bf.iter_set().collect();
            assert!(bits.is_empty());

            // Set some bits
            bf.set(0).unwrap();
            bf.set(3).unwrap();
            bf.set(9).unwrap();

            let bits: Vec<usize> = bf.iter_set().collect();
            assert_eq!(bits, vec![0, 3, 9]);

            // Set all bits
            for i in 0..10 {
                bf.set(i).unwrap();
            }
            let bits: Vec<usize> = bf.iter_set().collect();
            assert_eq!(bits, (0..10).collect::<Vec<_>>());
        }
    }
}
