// See [reference](https://blog.libtorrent.org/2011/11/writing-a-fast-piece-picker/)
//

use std::{collections::HashMap, sync::Arc};

use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::Block;

use crate::bitfield::Bitfield;

pub struct PiecePicker {
    pieces: Vec<Piece>,
}

#[derive(Debug)]
pub enum DownloadTask {
    /// Indicated downloading a whole piece
    Piece(u32),
    // Indicated downloading a block subset from a piece
    Blocks(std::ops::Range<usize>),
    // Inidicated downlaoding a single block, probably we are in end-game
    Block,
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    index: u32,
    /// Piece availabilty in the swarm
    num_peers: usize,
    partial: bool,
}

impl PiecePicker {
    pub fn new(torrent_info: Arc<Info>) -> Self {
        let num_pieces = torrent_info.pieces.len();
        let pieces = (0..num_pieces)
            .map(|piece_idx| Piece {
                index: piece_idx as u32,
                num_peers: 0,
                partial: false,
            })
            .collect();

        Self { pieces }
    }

    /// bitfiled indicated the pieces remote peer has
    /// num_bytes: represent the size in bytes that the remote peer need for a efficient outstanding block requests to keep to a peer
    pub fn pick_piece(&mut self, bitfield: &Bitfield, num_bytes: usize) -> Option<DownloadTask> {
        let pieces_peer_has = bitfield.iter_set();
        todo!("select a piece based on peer bitifeld and num_bytes")
    }
}

// This controlls Collect Block/Piece
// It will be used to pick pieces
// and to write complete pieces into Storage
type PieceIndex = u32;

/// Tracks completed byte ranges for a piece and manages the piece buffer
#[derive(Debug, Clone)]
struct PieceMetadata {
    /// Fixed-size piece buffer
    piece_bytes: Box<[u8]>,
    /// Completed byte ranges, kept merged and sorted
    completed_ranges: Vec<std::ops::Range<u32>>,
    /// Total expected length of this piece
    expected_length: u32,
}

impl PieceMetadata {
    /// Create a new piece metadata with the given expected length
    pub fn new(piece_len: usize) -> Self {
        Self {
            piece_bytes: vec![0u8; piece_len].into_boxed_slice(),
            completed_ranges: Vec::new(),
            expected_length: piece_len as u32,
        }
    }

    /// Add a block to the piece, merging ranges and returning the complete piece if finished
    pub fn add_block(&mut self, block: &Block) -> bool {
        let offset = block.begin;
        let length = block.data.len() as u32;
        let end = offset + length;

        // Validate block bounds
        if end > self.expected_length {
            return false;
        }

        // Copy data into the piece buffer
        let start_idx = offset as usize;
        let end_idx = end as usize;
        self.piece_bytes[start_idx..end_idx].copy_from_slice(&block.data);

        // Add and merge the new range
        self.add_range(offset..end);

        // Check if piece is complete
        self.is_complete()
    }

    /// Check if the piece is fully complete
    pub fn is_complete(&self) -> bool {
        self.completed_ranges.len() == 1
            && self.completed_ranges[0].start == 0
            && self.completed_ranges[0].end == self.expected_length
    }

    /// Get missing byte ranges that haven't been received yet
    pub fn get_missing_ranges(&self) -> Vec<std::ops::Range<u32>> {
        if self.completed_ranges.is_empty() {
            return vec![0..self.expected_length];
        }

        let mut missing = Vec::new();
        let mut current_pos = 0;

        for range in &self.completed_ranges {
            if current_pos < range.start {
                missing.push(current_pos..range.start);
            }
            current_pos = range.end;
        }

        // Check if there's a gap at the end
        if current_pos < self.expected_length {
            missing.push(current_pos..self.expected_length);
        }

        missing
    }

    /// Add a range and merge overlapping/adjacent ranges
    fn add_range(&mut self, new_range: std::ops::Range<u32>) {
        // Find insertion point and merge overlapping ranges
        let mut merged_range = new_range;
        let mut insert_pos = 0;
        let mut ranges_to_remove = Vec::new();

        for (i, existing_range) in self.completed_ranges.iter().enumerate() {
            if merged_range.end < existing_range.start {
                // New range comes before this one, no overlap
                insert_pos = i;
                break;
            } else if merged_range.start <= existing_range.end {
                // Ranges overlap or are adjacent, merge them
                merged_range.start = merged_range.start.min(existing_range.start);
                merged_range.end = merged_range.end.max(existing_range.end);
                ranges_to_remove.push(i);
            } else {
                // New range comes after this one
                insert_pos = i + 1;
            }
        }

        // Remove merged ranges in reverse order to maintain indices
        for &i in ranges_to_remove.iter().rev() {
            self.completed_ranges.remove(i);
            if i < insert_pos {
                insert_pos -= 1;
            }
        }

        // Insert the merged range
        self.completed_ranges.insert(insert_pos, merged_range);
    }

    pub fn into_bytes(self) -> Box<[u8]> {
        self.piece_bytes
    }
}

pub struct PieceCollector {
    piece_map: HashMap<PieceIndex, PieceMetadata>,
}

impl PieceCollector {
    pub fn new(torrent_info: Arc<Info>) -> Self {
        let mut piece_map = HashMap::new();
        let num_pieces = torrent_info.pieces.len();

        for piece_idx in 0..num_pieces {
            let piece_len = torrent_info.get_piece_len(piece_idx) as usize;
            piece_map.insert(piece_idx, PieceMetadata::new(piece_len));
        }

        Self {
            piece_map: HashMap::new(),
        }
    }

    // return the subset of blocks of a piece which are not received yet
    pub fn get_empty_blocks(&self, piece_idx: u32) -> Vec<std::ops::Range<u32>> {
        self.piece_map[&piece_idx].get_missing_ranges()
    }

    /// Add a block to the corresponding piece buffer
    /// Returns Some(Box<[u8]>) if the piece is completed after adding this block
    /// and remove entry from cache
    pub fn add_block(&mut self, block: Block) -> Option<Box<[u8]>> {
        let idx = block.index;
        if let Some(meta) = self.piece_map.get_mut(&idx)
            && meta.add_block(&block)
        {
            return self.piece_map.remove(&idx).map(PieceMetadata::into_bytes);
        }
        None
    }
}

#[cfg(test)]
mod test {
    use bittorrent_common::metainfo::parse_torrent_from_file;

    use super::*;
    #[test]
    fn test_correct_amount_of_pieces() {
        let torrent =
            parse_torrent_from_file("../sample_torrents/sample.torrent").expect("parse torrent");
        let t = torrent.info;
        let piece_picker = PiecePicker::new(t.clone());

        assert_eq!(piece_picker.pieces.len(), t.pieces.len())
    }
}
