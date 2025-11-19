use std::sync::Arc;

use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::Block;

use crate::{bitfield::Bitfield, torrent::Pid};

// Tracks
// Availability of pieces
// State of each piece at block-level
// Piece Buffer
//
type PieceIndex = usize;

#[derive(Debug, Eq, PartialEq, PartialOrd, Ord)]
struct PieceAvailability {
    npeers: usize,
    piece_index: PieceIndex,
}

pub struct PieceManager {
    pieces: Vec<PieceMetadata>,
}

struct PieceMetadata {
    index: PieceIndex,
    num_peer: usize,
    state: PieceState,
    buffer: Option<PieceBuffer>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum PieceState {
    NotRequested,
    Request(Pid),
    Downloaded,
    Have,
}

pub enum AvailabilityUpdate<'a> {
    Bitfield(&'a Bitfield),
    Have(u32),
}

impl PieceManager {
    pub fn new(torrent_info: Arc<Info>) -> Self {
        // torrent_info.num_pieces();
        let pieces: Vec<_> = (0..torrent_info.num_pieces())
            .map(|i| PieceMetadata {
                index: i,
                num_peer: 0,
                state: PieceState::NotRequested,
                buffer: Some(PieceBuffer::new(torrent_info.get_piece_len(i) as usize)),
            })
            .collect();

        Self { pieces }
    }

    pub fn increment_availability(&mut self, update: AvailabilityUpdate) {
        match update {
            AvailabilityUpdate::Bitfield(bitfield) => {
                for index in bitfield.iter_set() {
                    if let Some(piece) = self.pieces.get_mut(index) {
                        piece.num_peer += 1
                    }
                }
            }
            AvailabilityUpdate::Have(index) => {
                if let Some(piece) = self.pieces.get_mut(index as usize) {
                    piece.num_peer += 1
                }
            }
        }
    }

    pub fn decrement_availability(&mut self, update: AvailabilityUpdate) {
        match update {
            AvailabilityUpdate::Bitfield(bitfield) => {
                for index in bitfield.iter_set() {
                    if let Some(piece) = self.pieces.get_mut(index) {
                        piece.num_peer -= 1
                    }
                }
            }
            AvailabilityUpdate::Have(index) => {
                if let Some(piece) = self.pieces.get_mut(index as usize) {
                    piece.num_peer -= 1
                }
            }
        }
    }

    pub fn clear_pieces_by_peer(&mut self, peer_id: Pid) {}

    pub fn set_piece_as(&mut self, index: PieceIndex, state: PieceState) {}

    pub fn pick_piece(&mut self, remote_bitfield: Bitfield, peer_id: Pid) -> Option<u32> {
        for peer_has in remote_bitfield.iter_set() {
            if self.pieces[peer_has].state == PieceState::NotRequested {
                return Some(peer_has as u32);
            }
        }

        None
    }

    pub fn add_block(&mut self, block: Block) -> Option<Box<[u8]>> {
        let piece_index = block.index;
        let piece = self.pieces.get_mut(piece_index as usize)?;

        let buffer = piece.buffer.as_mut()?;

        if buffer.add_block(&block) {
            piece.state = PieceState::Downloaded;
            return piece.buffer.take().map(|b| b.into_bytes());
        }

        None
    }
}

struct PieceBuffer {
    piece_bytes: Box<[u8]>,
    completed_ranges: Vec<std::ops::Range<u32>>,
    expected_length: u32,
}

impl PieceBuffer {
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

    // Is Box<[u8]> representative for what we need?
    pub fn into_bytes(self) -> Box<[u8]> {
        self.piece_bytes
    }
}
