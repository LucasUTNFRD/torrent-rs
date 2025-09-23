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
            Metadata::MagnetUri { metadata_state, .. } => match metadata_state {
                MetadataState::Fetching {
                    total_pieces,
                    metadata_size,
                    buf,
                    metadata_pieces,
                    ..
                } => {
                    let piece_idx = piece_idx as usize;

                    // Validate piece index
                    if piece_idx >= *total_pieces {
                        return Err(format!(
                            "Piece index {} out of bounds (total pieces: {})",
                            piece_idx, total_pieces
                        ));
                    }

                    // Check if we already have this piece
                    if metadata_pieces[piece_idx].have {
                        return Ok(()); // Already have this piece, ignore duplicate
                    }

                    const PIECE_SIZE: usize = 16 * 1024; // 16KiB
                    let offset = piece_idx * PIECE_SIZE;

                    // Calculate the actual piece size for this piece
                    let remaining_bytes = *metadata_size - offset;
                    let expected_piece_size = std::cmp::min(PIECE_SIZE, remaining_bytes);

                    // Validate piece size
                    if metadata_piece.len() != expected_piece_size {
                        return Err(format!(
                            "Invalid piece size: expected {}, got {}",
                            expected_piece_size,
                            metadata_piece.len()
                        ));
                    }

                    // Validate buffer bounds
                    if offset + metadata_piece.len() > buf.len() {
                        return Err(format!(
                            "Piece data would overflow buffer: offset {} + size {} > buffer size {}",
                            offset,
                            metadata_piece.len(),
                            buf.len()
                        ));
                    }

                    // Copy the piece data into the buffer
                    buf[offset..offset + metadata_piece.len()].copy_from_slice(&metadata_piece);

                    Ok(())
                }
                _ => Err("Cannot put metadata piece when not in Fetching state".to_string()),
            },
            Metadata::TorrentFile(_) => {
                Err("Cannot put metadata piece for torrent file".to_string())
            }
        }
    }

    pub fn metadata_request_reject(&mut self, piece_idx: u32) -> Result<(), String> {
        match self {
            Metadata::MagnetUri { metadata_state, .. } => match metadata_state {
                MetadataState::Fetching {
                    metadata_pieces, ..
                } => {
                    if let Some(piece) = metadata_pieces.get_mut(piece_idx as usize) {
                        piece.num_req = piece.num_req.saturating_sub(1);
                        piece.time_metadata_request = Instant::now();
                        Ok(())
                    } else {
                        Err("out of bound metadata piece index".to_string())
                    }
                }
                _ => Err("Cannot mark metadata piece when not in Fetching state".to_string()),
            },
            Metadata::TorrentFile(_) => {
                Err("Cannot mark metadata piece for torrent file".to_string())
            }
        }
    }

    pub fn mark_metadata_piece(&mut self, piece_idx: u32) -> Result<bool, String> {
        match self {
            Metadata::MagnetUri { metadata_state, .. } => match metadata_state {
                MetadataState::Fetching {
                    pieces_received,
                    total_pieces,
                    metadata_pieces,
                    ..
                } => {
                    let piece_idx = piece_idx as usize;

                    // Validate piece index
                    if piece_idx >= *total_pieces {
                        return Err(format!(
                            "Piece index {} out of bounds (total pieces: {})",
                            piece_idx, total_pieces
                        ));
                    }

                    // Check if we already had this piece
                    if metadata_pieces[piece_idx].have {
                        return Ok(false); // Already had this piece, no change
                    }

                    // Mark the piece as received
                    metadata_pieces[piece_idx].have = true;
                    *pieces_received += 1;

                    // Check if we have all pieces
                    let all_pieces_received = *pieces_received == *total_pieces;

                    Ok(all_pieces_received)
                }
                _ => Err("Cannot mark metadata piece when not in Fetching state".to_string()),
            },
            Metadata::TorrentFile(_) => {
                Err("Cannot mark metadata piece for torrent file".to_string())
            }
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Metadata::TorrentFile(_) => true, // Torrent files are always valid
            Metadata::MagnetUri {
                magnet,
                metadata_state,
            } => match metadata_state {
                MetadataState::Fetching {
                    buf,
                    metadata_size,
                    pieces_received,
                    total_pieces,
                    ..
                } => {
                    // Only validate if we have all pieces
                    if *pieces_received != *total_pieces {
                        return false;
                    }

                    // Validate that buffer has correct size
                    if buf.len() != *metadata_size {
                        return false;
                    }

                    // Calculate hash of the complete metadata
                    let expected_info_hash = magnet.info_hash().expect("info_hash is mandatory");
                    let mut hasher = Sha1::new();
                    hasher.update(buf);
                    let calculated_hash: [u8; 20] = hasher.finalize().into();
                    let info_hash = InfoHash::new(calculated_hash);

                    expected_info_hash == info_hash
                }
                MetadataState::Complete(_) => true,
                MetadataState::Pending => false,
            },
        }
    }

    /// Validate metadata hash without borrowing self
    fn validate_metadata_hash(magnet: &Magnet, buf: &[u8]) -> bool {
        let expected_info_hash = magnet.info_hash().expect("info_hash is mandatory");
        let mut hasher = Sha1::new();
        hasher.update(buf);
        let calculated_hash: [u8; 20] = hasher.finalize().into();
        let info_hash = InfoHash::new(calculated_hash);
        expected_info_hash == info_hash
    }

    pub fn construct_info(&mut self) -> Result<(), String> {
        match self {
            Metadata::MagnetUri {
                magnet,
                metadata_state,
            } => match metadata_state {
                MetadataState::Fetching {
                    buf,
                    pieces_received,
                    total_pieces,
                    ..
                } => {
                    // Ensure we have all pieces
                    if *pieces_received != *total_pieces {
                        return Err("Cannot construct info: not all pieces received".to_string());
                    }

                    // Validate metadata hash
                    if !Self::validate_metadata_hash(magnet, buf) {
                        return Err("Cannot construct info: metadata validation failed".to_string());
                    }

                    // Parse the bencode data to construct Info
                    let info_bencode = bencode::Bencode::decode(buf)
                        .map_err(|e| format!("Failed to decode metadata bencode: {}", e))?;

                    // Use the parse_info_dict function from metainfo module
                    let info_struct =
                        bittorrent_common::metainfo::parse_info_dict(&info_bencode)
                            .map_err(|e| format!("Failed to parse Info from bencode: {}", e))?;

                    // Transition to Complete state
                    *metadata_state = MetadataState::Complete(info_struct);

                    Ok(())
                }
                MetadataState::Complete(_) => {
                    Ok(()) // Already complete
                }
                MetadataState::Pending => {
                    Err("Cannot construct info: metadata is still pending".to_string())
                }
            },
            Metadata::TorrentFile(_) => {
                Err("Cannot construct info for torrent file (already has info)".to_string())
            }
        }
    }

    /// Initialize metadata fetching with the given size
    pub fn set_metadata_size(&mut self, metadata_size: usize) -> Result<(), String> {
        match self {
            Metadata::MagnetUri { metadata_state, .. } => {
                if !matches!(metadata_state, MetadataState::Pending) {
                    return Err("Can only set metadata size when in Pending state".to_string());
                }

                if metadata_size == 0 {
                    return Err("Metadata size cannot be zero".to_string());
                }

                // Calculate number of 16KiB pieces needed
                const PIECE_SIZE: usize = 16 * 1024; // 16KiB
                let total_pieces = metadata_size.div_ceil(PIECE_SIZE);

                let metadata_pieces = (0..total_pieces)
                    .map(|_| MetadataPiece {
                        num_req: 0,
                        time_metadata_request: Instant::now(),
                        have: false,
                    })
                    .collect();

                *metadata_state = MetadataState::Fetching {
                    pieces_received: 0,
                    total_pieces,
                    metadata_size,
                    buf: BytesMut::zeroed(metadata_size),
                    metadata_pieces,
                };

                Ok(())
            }
            Metadata::TorrentFile(_) => {
                Err("Cannot set metadata size for torrent file".to_string())
            }
        }
    }

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
