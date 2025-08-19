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
use peer_protocol::protocol::{Block, BlockInfo, Message};
use picker::{AvailabilityUpdate, Picker, PieceState};
use piece_cache::PieceCache;
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
    AddBlock(Block),
}

#[derive(Debug)]
pub enum PeerCommand {
    SendMessage(Message),
    Disconnect,
    AvailableTask(Vec<BlockInfo>),
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
    picker: Picker,
    cache: PieceCache,
    //choker
}

#[derive(Debug, Clone)]
struct PeerState {
    pub pid: Id,
    pub sender: mpsc::Sender<PeerCommand>,
    pub bitfield: Bitfield,
    pub am_interested: bool,
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
            cache: PieceCache::new(torrent.clone()),
            bitfield: Bitfield::new(torrent.num_pieces()),
            picker: Picker::new(torrent.clone()),
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
                        am_interested: false,
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
                    return;
                }

                self.picker
                    .increment_availability(AvailabilityUpdate::Index(piece_idx));

                if !peer.am_interested
                    && self
                        .picker
                        .send_interest(AvailabilityUpdate::Bitfield(&peer.bitfield))
                {
                    let _ = peer
                        .sender
                        .send(PeerCommand::SendMessage(Message::Interested))
                        .await;

                    peer.am_interested = true;

                    if let Some((idx, piece)) = self.picker.pick_piece(&peer.bitfield) {
                        let _ = peer.sender.send(PeerCommand::AvailableTask(piece));
                        self.picker.mark_piece_as(idx, PieceState::Requested);
                        //TODO: Add piece request expiration policy
                    }
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
                    Ok(bitfield) => {
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

                        // determine if we are interested
                        if self
                            .picker
                            .send_interest(AvailabilityUpdate::Bitfield(&peer.bitfield))
                        {
                            let _ = peer
                                .sender
                                .send(PeerCommand::SendMessage(Message::Interested))
                                .await;
                            peer.am_interested = true;

                            if let Some((idx, piece)) = self.picker.pick_piece(&peer.bitfield) {
                                // peer.sender.send(PeerCommand::Request(piece))
                                self.picker.mark_piece_as(idx, PieceState::Requested);
                                //TODO: Add piece request expiration policy
                            }
                        }
                        // and if so set download queue
                    }
                    Err(e) => {
                        tracing::warn!("dropping peer connection - reason {e}");
                        // POTENTIAL BUG: We are dropping connection and remove peer entry in this
                        // block
                        let peer = self.peers.remove(&pid).unwrap();
                        let _ = peer.sender.send(PeerCommand::Disconnect).await;
                    }
                };
            }
            AddBlock(block) => {
                // cache the block
                if let Some(completed_piece) = self.cache.insert_block(&block) {
                    self.picker
                        .mark_piece_as(block.index as usize, PieceState::Writing);

                    todo!("Validate piece")
                }
            }
            PeerError(id, err) => {}
        }
    }
}

mod picker {
    use std::sync::Arc;

    use bittorrent_core::metainfo::TorrentInfo;
    use peer_protocol::protocol::BlockInfo;

    use super::bitfield::Bitfield;

    pub const BLOCK_SIZE: u32 = 1 << 14;

    // reference: https://blog.libtorrent.org/2011/11/writing-a-fast-piece-picker/
    pub struct Picker {
        torrent: Arc<TorrentInfo>,
        num_pieces: usize,
        piece_availability: Vec<PieceIndex>,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    struct PieceIndex {
        availabilty: usize,
        partial: bool,
        state: PieceState,
        size: usize,
        // blocks: Vec<BlockInfo>,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub enum PieceState {
        None,
        Requested,
        Writing,
        Finished,
    }

    pub enum AvailabilityUpdate<'a> {
        Bitfield(&'a Bitfield),
        Index(u32),
    }

    impl Picker {
        pub fn new(torrent: Arc<TorrentInfo>) -> Self {
            let total_pieces = torrent.num_pieces();
            let pieces = (0..total_pieces)
                .map(|piece_idx| PieceIndex {
                    availabilty: 0,
                    partial: false,
                    state: PieceState::None,
                    size: torrent.get_piece_len(piece_idx) as usize,
                    // blocks:
                })
                .collect();
            Self {
                torrent,
                num_pieces: total_pieces,
                piece_availability: pieces,
            }
        }

        pub fn increment_availability(&mut self, update: AvailabilityUpdate) {
            match update {
                AvailabilityUpdate::Bitfield(bitfield) => {
                    if bitfield.all_set() {
                        self.piece_availability
                            .iter_mut()
                            .for_each(|p| p.availabilty += 1);
                    } else {
                        for idx in bitfield.iter_set() {
                            self.piece_availability[idx].availabilty += 1;
                        }
                    }
                }
                AvailabilityUpdate::Index(idx) => {
                    if let Some(p) = self.piece_availability.get_mut(idx as usize) {
                        p.availabilty += 1;
                    }
                }
            }
        }

        // returns true if peer has a piece we dont
        // this could be called when the peer sends their bitifled
        // or the peer sent his bitfield and previously we were not interested
        // and now he sent a have, and we could be now interested
        pub fn send_interest(&self, update: AvailabilityUpdate) -> bool {
            match update {
                AvailabilityUpdate::Bitfield(bitfield) => {
                    for idx in bitfield.iter_set() {
                        if self.piece_availability[idx].state == PieceState::None {
                            return true;
                        }
                    }
                    false
                }
                AvailabilityUpdate::Index(idx) => {
                    self.piece_availability[idx as usize].state == PieceState::None
                }
            }
        }

        pub fn decrement_availability(&mut self, update: AvailabilityUpdate) {
            match update {
                AvailabilityUpdate::Bitfield(bitfield) => {
                    if bitfield.all_set() {
                        self.piece_availability
                            .iter_mut()
                            .for_each(|p| p.availabilty = p.availabilty.saturating_sub(1));
                    } else {
                        for idx in bitfield.iter_set() {
                            let p = &mut self.piece_availability[idx];
                            p.availabilty = p.availabilty.saturating_sub(1);
                        }
                    }
                }
                AvailabilityUpdate::Index(idx) => {
                    if let Some(p) = self.piece_availability.get_mut(idx as usize) {
                        p.availabilty = p.availabilty.saturating_sub(1)
                    }
                }
            }
        }

        fn get_blocks(&self, piece_idx: usize) -> Vec<BlockInfo> {
            let piece_size = self.piece_availability[piece_idx].size;
            (0..piece_size)
                .step_by(BLOCK_SIZE as usize)
                .map(|offset| BlockInfo {
                    index: piece_idx as u32,
                    begin: offset as u32,
                    length: BLOCK_SIZE.min(piece_size as u32 - offset as u32),
                })
                .collect()
        }

        /// Called of this function, Must call Mark_as_downloading
        pub fn pick_piece(&self, peer_bitfield: &Bitfield) -> Option<(usize, Vec<BlockInfo>)> {
            // Rarest-first among pieces we don't have (state == None) and the peer has
            let piece_idx = peer_bitfield
                .iter_set()
                .filter(|&i| self.piece_availability[i].state == PieceState::None)
                .min_by_key(|&i| self.piece_availability[i].availabilty)?;

            // let piece_idx = candidate.first()?;
            let blocks = self.get_blocks(piece_idx);

            Some((piece_idx, blocks))
        }

        pub fn mark_piece_as(&mut self, piece_idx: usize, state: PieceState) {
            if let Some(piece) = self.piece_availability.get_mut(piece_idx) {
                piece.state = state;
            }
        }
    }

    // TODO: Add unit test for picker
}

mod piece_cache {
    use std::{collections::HashMap, sync::Arc};

    use bittorrent_core::metainfo::TorrentInfo;
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
                let buffer = vec![0; piece_length as usize].into_boxed_slice();
                let piece_metadata = PieceMetadata {
                    buffer,
                    piece_length,
                    downloaded: 0,
                };
                pieces.insert(i, piece_metadata);
            }
            Self { pieces }
        }

        pub fn insert_block(&mut self, block: &Block) -> Option<Box<[u8]>> {
            let index = block.index as usize;

            let piece = self.pieces.get_mut(&index)?;
            let offset = block.begin as usize;
            piece.buffer[offset..offset + block.data.len()].copy_from_slice(&block.data);
            // BUG: WE overcount if presence of duplicates
            piece.downloaded += block.data.len();
            if piece.downloaded >= piece.piece_length {
                // Remove the piece from the cache
                let piece = self.pieces.remove(&index).unwrap();
                return Some(piece.buffer);
            }
            None
        }
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
                word_idx: 0,
                bit_mask: 0x80,
            }
        }
    }

    pub struct BitfieldSetIter<'a> {
        bitfield: &'a Bitfield,
        word_idx: usize,
        bit_mask: u8,
    }

    impl Iterator for BitfieldSetIter<'_> {
        type Item = usize;

        /// Yields Index of piece marked in bitfield
        fn next(&mut self) -> Option<Self::Item> {
            while self.word_idx < self.bitfield.bits.len() {
                let word = self.bitfield.bits[self.word_idx];
                while self.bit_mask != 0 {
                    if word & self.bit_mask != 0 {
                        let idx = self.word_idx * 32 + self.bit_mask.leading_zeros() as usize;
                        self.bit_mask >>= 1;
                        if idx < self.bitfield.nbits {
                            return Some(idx);
                        }
                    } else {
                        self.bit_mask >>= 1;
                    }
                }
                self.word_idx += 1;
                self.bit_mask = 0x80;
            }
            None
        }
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
