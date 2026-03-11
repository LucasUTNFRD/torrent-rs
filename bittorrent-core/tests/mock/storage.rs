use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bittorrent_common::metainfo::Info;
use bittorrent_common::types::InfoHash;
use bittorrent_core::storage::{StorageBackend, StorageError};
use bytes::Bytes;
use peer_protocol::protocol::{Block, BlockInfo};

/// Deterministic block generation following libtorrent's test_disk_io pattern.
///
/// Each 16 KiB block is filled with a 4-byte repeating pattern derived from
/// `(piece_index << 8) | (block_index & 0xFF)`.
pub fn generate_block(piece: u32, block: u32) -> Vec<u8> {
    let val = (piece << 8) | (block & 0xFF);
    let bytes = val.to_ne_bytes();
    let mut buffer = Vec::with_capacity(16384);
    for _ in 0..4096 {
        buffer.extend_from_slice(&bytes);
    }
    buffer
}

/// Generate a full piece from deterministic block data.
pub fn generate_piece(piece_index: u32, piece_length: u32) -> Vec<u8> {
    let block_size: u32 = 16384;
    let num_blocks = piece_length.div_ceil(block_size);
    let mut data = Vec::with_capacity(piece_length as usize);
    for b in 0..num_blocks {
        let block = generate_block(piece_index, b);
        let take = ((piece_length as usize) - data.len()).min(block.len());
        data.extend_from_slice(&block[..take]);
    }
    data
}

/// Internal per-torrent state for the mock.
struct MockTorrentState {
    meta: Arc<Info>,
    /// piece_index -> piece data
    pieces: HashMap<u32, Vec<u8>>,
}

/// In-memory storage backend for simulation testing.
///
/// - Writes store piece data in a `HashMap`
/// - Reads return stored data
/// - Verification computes SHA-1 against the torrent's piece hashes
pub struct MockStorage {
    state: Mutex<HashMap<InfoHash, MockTorrentState>>,
}

impl MockStorage {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Create a MockStorage pre-populated with all pieces for the given torrent.
    /// Used for seeder nodes in simulation tests.
    #[allow(dead_code)]
    pub fn all_have(info_hash: InfoHash, meta: Arc<Info>) -> Self {
        let mut pieces = HashMap::new();
        let num_pieces = meta.num_pieces();
        for i in 0..num_pieces {
            let piece_len = meta.get_piece_len(i);
            pieces.insert(i as u32, generate_piece(i as u32, piece_len));
        }

        let torrent_state = MockTorrentState {
            meta: meta.clone(),
            pieces,
        };

        let mut map = HashMap::new();
        map.insert(info_hash, torrent_state);

        Self {
            state: Mutex::new(map),
        }
    }
}

#[async_trait]
impl StorageBackend for MockStorage {
    async fn add_torrent(
        &self,
        info_hash: InfoHash,
        torrent: Arc<Info>,
    ) -> Result<(), StorageError> {
        let mut state = self.state.lock().unwrap();
        state.insert(
            info_hash,
            MockTorrentState {
                meta: torrent,
                pieces: HashMap::new(),
            },
        );
        Ok(())
    }

    async fn add_seed(
        &self,
        info_hash: InfoHash,
        torrent: Arc<Info>,
        _content_dir: PathBuf,
    ) -> Result<(), StorageError> {
        // For seeding, populate all pieces with deterministic data
        let num_pieces = torrent.num_pieces();
        let mut pieces = HashMap::new();
        for i in 0..num_pieces {
            let piece_len = torrent.get_piece_len(i);
            pieces.insert(i as u32, generate_piece(i as u32, piece_len));
        }

        let mut state = self.state.lock().unwrap();
        state.insert(
            info_hash,
            MockTorrentState {
                meta: torrent,
                pieces,
            },
        );
        Ok(())
    }

    async fn remove_torrent(&self, torrent_id: InfoHash) -> Result<(), StorageError> {
        let mut state = self.state.lock().unwrap();
        state.remove(&torrent_id);
        Ok(())
    }

    async fn verify_piece(
        &self,
        info_hash: InfoHash,
        piece_index: u32,
        piece_data: Arc<[u8]>,
    ) -> Result<bool, StorageError> {
        let state = self.state.lock().unwrap();
        let torrent = state
            .get(&info_hash)
            .ok_or(StorageError::TorrentNotFound(info_hash))?;

        let expected = torrent
            .meta
            .get_piece_hash(piece_index as usize)
            .ok_or(StorageError::PieceNotFound(piece_index))?;

        use sha1::{Digest, Sha1};
        let mut hasher = Sha1::new();
        hasher.update(&piece_data);
        let hash: [u8; 20] = hasher.finalize().into();

        Ok(hash == *expected)
    }

    async fn write_piece(
        &self,
        info_hash: InfoHash,
        piece_index: u32,
        piece_data: Arc<[u8]>,
    ) -> Result<(), StorageError> {
        let mut state = self.state.lock().unwrap();
        let torrent = state
            .get_mut(&info_hash)
            .ok_or(StorageError::TorrentNotFound(info_hash))?;

        torrent.pieces.insert(piece_index, piece_data.to_vec());
        Ok(())
    }

    async fn read_block(
        &self,
        torrent_id: InfoHash,
        block_info: BlockInfo,
    ) -> Result<Block, StorageError> {
        let state = self.state.lock().unwrap();
        let torrent = state
            .get(&torrent_id)
            .ok_or(StorageError::TorrentNotFound(torrent_id))?;

        let piece_data = torrent
            .pieces
            .get(&block_info.index)
            .ok_or(StorageError::PieceNotFound(block_info.index))?;

        let begin = block_info.begin as usize;
        let end = begin + block_info.length as usize;

        if end > piece_data.len() {
            return Err(StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Block extends beyond piece data",
            )));
        }

        Ok(Block {
            index: block_info.index,
            begin: block_info.begin,
            data: Bytes::copy_from_slice(&piece_data[begin..end]),
        })
    }
}
