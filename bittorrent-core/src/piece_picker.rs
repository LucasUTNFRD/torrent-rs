// See [reference](https://blog.libtorrent.org/2011/11/writing-a-fast-piece-picker/)
//

use std::{collections::HashMap, sync::Arc};

use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::Block;

use crate::{
    bitfield::{self, Bitfield},
    torrent::Pid,
};

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
pub enum State {
    NotRequested,
    Requested,
    Received,
    Downloaded,
}

#[derive(Debug, Clone, Copy)]
//The operations of the piece picker are:
// pick one or more pieces for peer p (this is to determine what to download from a peer)
struct Piece {
    index: u32,
    /// Piece availabilty in the swarm
    num_peers: usize,
    partial: bool,
    state: State,
}

pub enum AvailabilityUpdate {
    Bitfield(Bitfield),
    Have(PieceIndex),
}

impl PiecePicker {
    pub fn new(torrent_info: Arc<Info>) -> Self {
        let num_pieces = torrent_info.pieces.len();
        let pieces = (0..num_pieces)
            .map(|piece_idx| Piece {
                index: piece_idx as u32,
                num_peers: 0,
                partial: false,
                state: State::NotRequested,
            })
            .collect();

        Self { pieces }
    }

    /// bitfiled indicated the pieces remote peer has
    /// num_bytes: represent the size in bytes that the remote peer need for a efficient outstanding block requests to keep to a peer
    pub fn pick_piece(&mut self, bitfield: &Bitfield, _num_bytes: usize) -> Option<DownloadTask> {
        let mut candidate_pieces: Vec<&Piece> = self
            .pieces
            .iter()
            .filter(|piece| {
                matches!(piece.state, State::NotRequested) && bitfield.has(piece.index as usize)
            })
            .collect();

        if candidate_pieces.is_empty() {
            return None;
        }

        // Sort by rarest first (lowest num_peers)
        candidate_pieces.sort_by(|a, b| a.num_peers.cmp(&b.num_peers));

        // Return the rarest piece as a download task
        Some(DownloadTask::Piece(candidate_pieces[0].index))
    }

    pub fn set_piece_as(&mut self, piece_index: usize, state: State) {
        self.pieces[piece_index].state = state
    }

    // increment availability counter for piece i (when a peer announces that it just completed downloading a new piece)
    // increment availability counters for all pieces of peer p (when a peer joins the swarm)
    pub fn increment_availability(&mut self, update: AvailabilityUpdate) {
        match update {
            AvailabilityUpdate::Bitfield(bitfield) => {
                for idx in bitfield.iter_set() {
                    self.pieces[idx].num_peers += 1;
                }
            }
            AvailabilityUpdate::Have(idx) => {
                if let Some(p) = self.pieces.get_mut(idx as usize) {
                    p.num_peers += 1;
                }
            }
        }
    }

    // decrement availability counters for all pieces of peer p (when a peer leaves the swarm)
    pub fn decrement_availability(&mut self, bitfield: &Bitfield) {
        for piece in bitfield.iter_set() {
            self.pieces[piece].num_peers -= 1;
        }
    }

    pub fn info_log(&self) {
        if self.pieces.is_empty() {
            tracing::info!("PiecePicker: No pieces configured");
            return;
        }

        let total = self.pieces.len();

        // Count pieces by state
        let not_requested = self
            .pieces
            .iter()
            .filter(|p| matches!(p.state, State::NotRequested))
            .count();
        let requested = self
            .pieces
            .iter()
            .filter(|p| matches!(p.state, State::Requested))
            .count();
        let received = self
            .pieces
            .iter()
            .filter(|p| matches!(p.state, State::Received))
            .count();
        let downloaded = self
            .pieces
            .iter()
            .filter(|p| matches!(p.state, State::Downloaded))
            .count();

        // Count partial pieces
        let partial_pieces = self.pieces.iter().filter(|p| p.partial).count();

        // Calculate availability statistics
        let peer_counts: Vec<usize> = self.pieces.iter().map(|p| p.num_peers).collect();
        let total_availability: usize = peer_counts.iter().sum();
        let min_availability = peer_counts.iter().min().copied().unwrap_or(0);
        let max_availability = peer_counts.iter().max().copied().unwrap_or(0);
        let avg_availability = if total > 0 {
            total_availability as f64 / total as f64
        } else {
            0.0
        };

        // Calculate percentages
        let downloaded_pct = (downloaded as f64 / total as f64) * 100.0;
        let progress_pct = ((downloaded + received) as f64 / total as f64) * 100.0;

        // Count pieces with no availability (unavailable in swarm)
        let unavailable_pieces = peer_counts.iter().filter(|&&count| count == 0).count();

        tracing::info!(
            "PiecePicker Status:\n\
         ├─ Progress: {}/{} pieces ({:.1}% complete, {:.1}% in progress)\n\
         ├─ States: {} not_requested, {} requested, {} received, {} downloaded\n\
         ├─ Partial: {} pieces have partial downloads\n\
         ├─ Availability: avg={:.1}, min={}, max={} peers per piece\n\
         └─ Swarm: {} pieces unavailable (0 peers)",
            downloaded + received,
            total,
            downloaded_pct,
            progress_pct,
            not_requested,
            requested,
            received,
            downloaded,
            partial_pieces,
            avg_availability,
            min_availability,
            max_availability,
            unavailable_pieces
        );
    }
}

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
            piece_map.insert(piece_idx as u32, PieceMetadata::new(piece_len));
        }

        Self { piece_map }
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
