//
// Torrent metadata Types
//

use std::sync::Arc;

use bittorrent_common::{
    metainfo::{Info, TorrentInfo},
    types::InfoHash,
};
use bytes::{Bytes, BytesMut};
use magnet_uri::Magnet;
use sha1::{Digest, Sha1};
use tokio::time::Instant;

/// Represents the metadata of a torrent and its current metadata state
#[derive(Debug, Clone)]
pub enum Metadata {
    /// Torrent created from a .torrent file with complete metadata
    TorrentFile(Arc<TorrentInfo>),
    /// Torrent created from a magnet URI, metadata may be incomplete
    MagnetUri {
        magnet: Magnet,
        /// Current state of metadata fetching
        metadata_state: MetadataState,
    },
}

impl Metadata {
    /// Check if this torrent has complete metadata
    pub fn has_metadata(&self) -> bool {
        match &self {
            Metadata::TorrentFile { .. } => true,
            Metadata::MagnetUri { metadata_state, .. } => {
                matches!(metadata_state, MetadataState::Complete(_))
            }
        }
    }

    // Get a piece index to request from a peer which has metadata
    pub fn get_piece(&mut self) -> Option<u32> {
        match self {
            Metadata::TorrentFile(_) => None,
            Metadata::MagnetUri { metadata_state, .. } => {
                match metadata_state {
                    MetadataState::Fetching {
                        metadata_pieces, ..
                    } => {
                        // Find the first piece that we don't have and hasn't been requested too many times
                        for (piece_idx, piece) in metadata_pieces.iter_mut().enumerate() {
                            if !piece.have && piece.num_req < 2 {
                                piece.num_req += 1;
                                piece.time_metadata_request = Instant::now();
                                return Some(piece_idx as u32);
                            }
                        }
                        // If all pieces have been requested too many times, retry the oldest one
                        if let Some((oldest_idx, oldest_piece)) = metadata_pieces
                            .iter_mut()
                            .enumerate()
                            .filter(|(_, piece)| !piece.have)
                            .min_by_key(|(_, piece)| piece.time_metadata_request)
                        {
                            oldest_piece.num_req += 1;
                            oldest_piece.time_metadata_request = Instant::now();
                            return Some(oldest_idx as u32);
                        }
                        None
                    }
                    _ => None,
                }
            }
        }
    }

    pub fn put_metadata_piece(
        &mut self,
        metadata_piece: Bytes,
        piece_idx: u32,
    ) -> Result<(), String> {
        match self {
            Metadata::MagnetUri {
                magnet,
                metadata_state,
            } => match metadata_state {
                MetadataState::Fetching {
                    pieces_received,
                    total_pieces,
                    metadata_size,
                    buf,
                    metadata_pieces,
                } => {
                    let piece_len = metadata_pieces.len();
                    let offset = piece_idx as usize * 16 * 1024; // 16KiB blocks

                    buf[offset..offset + piece_len].copy_from_slice(&metadata_piece);
                    return Ok(());
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
        Ok(())
    }

    pub fn mark_metadata_piece(&mut self, piece_idx: u32) -> Result<bool, String> {
        match self {
            Metadata::MagnetUri {
                magnet,
                metadata_state,
            } => match metadata_state {
                MetadataState::Fetching {
                    pieces_received,
                    total_pieces,
                    metadata_size,
                    buf,
                    metadata_pieces,
                } => {
                    metadata_pieces[piece_idx as usize].have = true;

                    if buf.len() == *metadata_size {
                        let have_all = metadata_pieces.iter().all(|p| p.have);
                        debug_assert!(have_all);
                        return Ok(true); // mean that we should perform validation
                    }
                }
                _ => panic!(),
            },
            _ => panic!(),
        }

        Ok(false)
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Metadata::MagnetUri {
                magnet,
                metadata_state,
            } => match metadata_state {
                MetadataState::Fetching {
                    pieces_received,
                    total_pieces,
                    metadata_size,
                    buf,
                    metadata_pieces,
                } => {
                    let expected_info_hash = self.info_hash();
                    let mut hasher = Sha1::new();
                    hasher.update(buf);
                    let piece_hash: [u8; 20] = hasher.finalize().into();
                    let info_hash = InfoHash::new(piece_hash);

                    expected_info_hash == info_hash
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    pub fn construct_info(&mut self) -> Result<(), String> {
        match self {
            Metadata::MagnetUri {
                magnet,
                metadata_state,
            } => match metadata_state {
                MetadataState::Fetching {
                    pieces_received,
                    total_pieces,
                    metadata_size,
                    buf,
                    metadata_pieces,
                } => {
                    todo!()
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    // pub fn set_metadata_size(&self, metadata_size: u32) -> Result<(), String> {
    //     todo!()
    // }

    /// Get the Info struct if available
    pub fn info(&self) -> Option<&Info> {
        match &self {
            Metadata::TorrentFile(torrent_info) => Some(&torrent_info.info),
            Metadata::MagnetUri { metadata_state, .. } => match metadata_state {
                MetadataState::Complete(info) => Some(info),
                _ => None,
            },
        }
    }

    pub fn info_hash(&self) -> InfoHash {
        match &self {
            Self::TorrentFile(info) => info.as_ref().info_hash,
            Self::MagnetUri { magnet, .. } => {
                magnet.info_hash().expect("info_hash is a mandatory field")
            }
        }
    }

    /// Get the display name for this torrent
    pub fn display_name(&self) -> Option<&str> {
        match &self {
            Metadata::TorrentFile(torrent_info) => Some(torrent_info.info.mode.name()),
            Metadata::MagnetUri { magnet, .. } => magnet.display_name.as_deref(),
        }
    }

    /// Check if this torrent needs metadata fetching
    pub fn needs_metadata(&self) -> bool {
        match &self {
            Metadata::TorrentFile(_) => false,
            Metadata::MagnetUri { metadata_state, .. } => {
                matches!(metadata_state, MetadataState::Pending)
            }
        }
    }
}
/// Represents the state of metadata when working with magnet URIs
#[derive(Debug, Clone)]
pub enum MetadataState {
    /// No metadata available yet, need to fetch from peers
    Pending,
    /// Currently fetching metadata from peers
    Fetching {
        /// Number of pieces already received
        pieces_received: usize,
        /// Total number of pieces expected
        total_pieces: usize,
        /// Total expected metadata size
        metadata_size: usize,
        /// buffer incoming metadata
        buf: BytesMut,
        metadata_pieces: Vec<MetadataPiece>,
    },
    /// Metadata successfully fetched
    Complete(Info),
}

#[derive(Debug, Clone)]
pub struct MetadataPiece {
    pub num_req: usize,
    pub time_metadata_request: Instant,
    pub have: bool,
}
