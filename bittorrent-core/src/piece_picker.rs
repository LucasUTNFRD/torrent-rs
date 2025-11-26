use std::{collections::HashMap, sync::Arc};

use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::{Block, BlockInfo};

use crate::bitfield::Bitfield;

// Standard block size for BitTorrent (16 KiB)
const BLOCK_SIZE: u32 = 16384;

type PieceIndex = usize;
type BlockOffset = u32;

/// Represents a unique block request
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockRequest {
    pub piece_index: u32,
    pub begin: u32,
    pub length: u32,
}

impl From<BlockInfo> for BlockRequest {
    fn from(info: BlockInfo) -> Self {
        Self {
            piece_index: info.index,
            begin: info.begin,
            length: info.length,
        }
    }
}

impl From<BlockRequest> for BlockInfo {
    fn from(req: BlockRequest) -> Self {
        Self {
            index: req.piece_index,
            begin: req.begin,
            length: req.length,
        }
    }
}

#[derive(Debug, Eq, PartialEq, PartialOrd, Ord)]
struct PieceAvailability {
    npeers: usize,
    piece_index: PieceIndex,
}

/// State of an individual block within a piece
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockState {
    /// Block has not been requested yet
    NotRequested,
    /// Block is currently being requested (heat = number of requests)
    Requested { heat: usize },
    /// Block has been downloaded
    Downloaded,
}

/// Metadata for tracking block-level state within a piece
struct BlockMetadata {
    offset: u32,
    length: u32,
    state: BlockState,
}

pub struct PieceManager {
    pieces: Vec<PieceMetadata>,
    /// Global heat map tracking how many times each block has been requested
    heat: HashMap<BlockRequest, usize>,
    /// Torrent metadata for calculations
    torrent_info: Arc<Info>,
}

struct PieceMetadata {
    index: PieceIndex,
    /// Number of peers that have this piece
    num_peer: usize,
    /// Overall piece state
    state: PieceState,
    /// Block-level tracking
    blocks: Vec<BlockMetadata>,
    /// Buffer for assembling the complete piece
    buffer: Option<PieceBuffer>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum PieceState {
    /// No blocks have been requested
    NotRequested,
    /// Some blocks are being requested or downloaded
    InProgress,
    /// All blocks downloaded, waiting for hash verification
    Downloaded,
    /// Piece verified and saved
    Have,
}

pub enum AvailabilityUpdate<'a> {
    Bitfield(&'a Bitfield),
    Have(u32),
}

impl PieceManager {
    pub fn new(torrent_info: Arc<Info>) -> Self {
        let pieces: Vec<_> = (0..torrent_info.num_pieces())
            .map(|i| {
                let piece_len = torrent_info.get_piece_len(i);
                let blocks = Self::create_blocks_for_piece(i, piece_len);

                PieceMetadata {
                    index: i,
                    num_peer: 0,
                    state: PieceState::NotRequested,
                    blocks,
                    buffer: Some(PieceBuffer::new(piece_len as usize)),
                }
            })
            .collect();

        Self {
            pieces,
            heat: HashMap::new(),
            torrent_info,
        }
    }

    pub fn have_all_pieces(&self) -> bool {
        self.pieces.iter().all(|p| p.state == PieceState::Have)
    }

    /// Create block metadata for a piece based on its length
    fn create_blocks_for_piece(piece_index: usize, piece_len: u32) -> Vec<BlockMetadata> {
        let mut blocks = Vec::new();
        let mut offset = 0;

        while offset < piece_len {
            let length = BLOCK_SIZE.min(piece_len - offset);
            blocks.push(BlockMetadata {
                offset,
                length,
                state: BlockState::NotRequested,
            });
            offset += length;
        }

        blocks
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
                        piece.num_peer = piece.num_peer.saturating_sub(1);
                    }
                }
            }
            AvailabilityUpdate::Have(index) => {
                if let Some(piece) = self.pieces.get_mut(index as usize) {
                    piece.num_peer = piece.num_peer.saturating_sub(1);
                }
            }
        }
    }

    /// Mark a piece as having a specific state
    pub fn set_piece_as(&mut self, index: PieceIndex, state: PieceState) {
        if let Some(piece) = self.pieces.get_mut(index) {
            piece.state = state;

            // If marking as Have, mark all blocks as downloaded
            if matches!(state, PieceState::Have) {
                for block in &mut piece.blocks {
                    block.state = BlockState::Downloaded;
                }
            }
        }
    }

    /// Register a block request and increment its heat
    pub fn add_request(&mut self, request: BlockRequest) -> bool {
        let piece_idx = request.piece_index as usize;

        if let Some(piece) = self.pieces.get_mut(piece_idx) {
            // Don't request blocks for pieces we already have
            if piece.state == PieceState::Have {
                return false;
            }

            // Find the corresponding block
            if let Some(block) = piece.blocks.iter_mut().find(|b| b.offset == request.begin) {
                // Increment heat
                let heat = self.heat.entry(request).or_insert(0);
                *heat += 1;

                // Update block state
                block.state = BlockState::Requested { heat: *heat };

                // Update piece state if needed
                if piece.state == PieceState::NotRequested {
                    piece.state = PieceState::InProgress;
                }

                return true;
            }
        }

        false
    }

    pub fn info_progress(&self) {
        let have = self
            .pieces
            .iter()
            .filter(|p| p.state == PieceState::Have)
            .count();
        let total = self.pieces.len();

        tracing::info!("Piece progress {have:?}/{total:?}");
    }

    /// Remove a block request and decrement its heat
    pub fn delete_request(&mut self, request: BlockRequest) {
        if let Some(heat) = self.heat.get_mut(&request) {
            if *heat <= 1 {
                self.heat.remove(&request);

                // Update block state to NotRequested if heat reaches 0
                if let Some(piece) = self.pieces.get_mut(request.piece_index as usize)
                    && let Some(block) = piece.blocks.iter_mut().find(|b| b.offset == request.begin)
                    && matches!(block.state, BlockState::Requested { .. })
                {
                    block.state = BlockState::NotRequested;
                }
            } else {
                *heat -= 1;

                // Update block heat
                if let Some(piece) = self.pieces.get_mut(request.piece_index as usize)
                    && let Some(block) = piece.blocks.iter_mut().find(|b| b.offset == request.begin)
                {
                    block.state = BlockState::Requested { heat: *heat };
                }
            }
        }
    }

    /// Get the current heat for a block request
    pub fn get_heat(&self, request: &BlockRequest) -> usize {
        self.heat.get(request).copied().unwrap_or(0)
    }

    /// Pick blocks to request based on piece availability and block heat
    /// Returns up to `max_requests` blocks to request
    fn pick_blocks(
        &mut self,
        remote_bitfield: &Bitfield,
        max_requests: usize,
        heat_thresholds: &[usize],
    ) -> Vec<BlockInfo> {
        let mut blocks = Vec::new();

        if max_requests == 0 {
            return blocks;
        }

        // Strategy:
        // 1. First, try to complete pieces already in progress (endgame mode friendly)
        // 2. Then, pick from rarest pieces
        // Use heat thresholds to avoid over-requesting the same blocks

        for &heat_threshold in heat_thresholds {
            if blocks.len() >= max_requests {
                break;
            }

            // Get pieces sorted by:
            // 1. In-progress pieces first (to complete them)
            // 2. Then by rarity (fewest peers)
            let mut piece_priorities: Vec<_> = self
                .pieces
                .iter()
                .enumerate()
                .filter(|(idx, piece)| {
                    // Skip pieces we already have
                    piece.state != PieceState::Have &&
                    // Only include pieces the remote peer has
                    remote_bitfield.has(*idx)
                })
                .map(|(idx, piece)| {
                    let in_progress = piece.state == PieceState::InProgress;
                    let pending_blocks = piece
                        .blocks
                        .iter()
                        .filter(|b| !matches!(b.state, BlockState::Downloaded))
                        .count();

                    (idx, in_progress, piece.num_peer, pending_blocks)
                })
                .collect();

            // Sort: in-progress first, then by rarity (fewer peers = higher priority), then by pending blocks
            piece_priorities.sort_by(|a, b| {
                b.1.cmp(&a.1) // in_progress (true first)
                    .then_with(|| a.2.cmp(&b.2)) // num_peer (fewer first - rarest)
                    .then_with(|| b.3.cmp(&a.3)) // pending_blocks (more first)
            });

            for (piece_idx, _, _, _) in piece_priorities {
                if blocks.len() >= max_requests {
                    break;
                }

                let piece = &self.pieces[piece_idx];

                // Get blocks from this piece that can be requested
                for block in &piece.blocks {
                    if blocks.len() >= max_requests {
                        break;
                    }

                    // Skip already downloaded blocks
                    if matches!(block.state, BlockState::Downloaded) {
                        continue;
                    }

                    let request = BlockRequest {
                        piece_index: piece_idx as u32,
                        begin: block.offset,
                        length: block.length,
                    };

                    // Check heat threshold
                    let heat = self.get_heat(&request);
                    if heat > heat_threshold {
                        continue;
                    }

                    blocks.push(BlockInfo {
                        index: piece_idx as u32,
                        begin: block.offset,
                        length: block.length,
                    });
                }
            }
        }

        blocks
    }

    /// Simplified version of pick_blocks with default heat thresholds
    pub fn pick_piece(
        &mut self,
        remote_bitfield: &Bitfield,
        max_requests: usize,
    ) -> Vec<BlockInfo> {
        // Use progressive heat thresholds like the Go implementation
        let heat_thresholds = [0, 1, 4, 100];
        self.pick_blocks(remote_bitfield, max_requests, &heat_thresholds)
    }

    /// Get all pending blocks for a piece (for debugging/monitoring)
    pub fn get_pending_blocks(&self, piece_index: u32) -> Vec<BlockInfo> {
        self.pieces
            .get(piece_index as usize)
            .map(|piece| {
                piece
                    .blocks
                    .iter()
                    .filter(|b| !matches!(b.state, BlockState::Downloaded))
                    .map(|b| BlockInfo {
                        index: piece_index,
                        begin: b.offset,
                        length: b.length,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Add a received block to its piece buffer
    /// Returns Some(Box<[u8>]) in case a piece is completed
    pub fn add_block(&mut self, block: Block) -> Option<Box<[u8]>> {
        let piece_index = block.index as usize;
        let piece = self.pieces.get_mut(piece_index)?;

        // Mark the block as downloaded
        if let Some(block_meta) = piece.blocks.iter_mut().find(|b| b.offset == block.begin) {
            block_meta.state = BlockState::Downloaded;

            // Remove from heat map
            let request = BlockRequest {
                piece_index: block.index,
                begin: block.begin,
                length: block.data.len() as u32,
            };
            self.heat.remove(&request);
        }

        let buffer = piece.buffer.as_mut()?;

        if buffer.add_block(&block) {
            piece.state = PieceState::Downloaded;
            return piece.buffer.take().map(|b| b.into_bytes());
        }

        None
    }

    /// Check if all blocks in a piece have been downloaded
    pub fn is_piece_complete(&self, piece_index: u32) -> bool {
        self.pieces
            .get(piece_index as usize)
            .map(|piece| {
                piece
                    .blocks
                    .iter()
                    .all(|b| matches!(b.state, BlockState::Downloaded))
            })
            .unwrap_or(false)
    }

    /// Get statistics about pending bytes per piece (for prioritization)
    pub fn pieces_by_pending_bytes(&self) -> Vec<(PieceIndex, usize)> {
        let mut pieces_with_pending: Vec<_> = self
            .pieces
            .iter()
            .filter(|p| p.state != PieceState::Have && p.state != PieceState::NotRequested)
            .map(|p| {
                let pending_bytes: usize = p
                    .blocks
                    .iter()
                    .filter(|b| !matches!(b.state, BlockState::Downloaded))
                    .map(|b| b.length as usize)
                    .sum();
                (p.index, pending_bytes)
            })
            .collect();

        // Sort by pending bytes (ascending - finish pieces with fewer pending bytes first)
        pieces_with_pending.sort_by_key(|(_, pending)| *pending);
        pieces_with_pending
    }

    /// Reset all block requests for a peer (called when peer disconnects)
    /// Returns the requests that need to be decremented
    pub fn cancel_peer_requests(&mut self, requests: &[BlockRequest]) {
        for request in requests {
            self.delete_request(*request);
        }
    }
    /// Print detailed debug information about all pieces and their blocks
    pub fn debug_print_state(&self) {
        println!("\n========== PIECE PICKER STATE DEBUG ==========");
        println!("Total Pieces: {}", self.pieces.len());
        println!("Total Heat Entries: {}", self.heat.len());

        // Summary statistics
        let mut total_blocks = 0;
        let mut not_requested_blocks = 0;
        let mut requested_blocks = 0;
        let mut downloaded_blocks = 0;

        let mut not_requested_pieces = 0;
        let mut in_progress_pieces = 0;
        let mut downloaded_pieces = 0;
        let mut have_pieces = 0;

        for piece in &self.pieces {
            match piece.state {
                PieceState::NotRequested => not_requested_pieces += 1,
                PieceState::InProgress => in_progress_pieces += 1,
                PieceState::Downloaded => downloaded_pieces += 1,
                PieceState::Have => have_pieces += 1,
            }

            for block in &piece.blocks {
                total_blocks += 1;
                match block.state {
                    BlockState::NotRequested => not_requested_blocks += 1,
                    BlockState::Requested { .. } => requested_blocks += 1,
                    BlockState::Downloaded => downloaded_blocks += 1,
                }
            }
        }

        println!("\n--- Piece Summary ---");
        println!("  NotRequested: {}", not_requested_pieces);
        println!("  InProgress:   {}", in_progress_pieces);
        println!("  Downloaded:   {}", downloaded_pieces);
        println!("  Have:         {}", have_pieces);

        println!("\n--- Block Summary ---");
        println!("  Total:        {}", total_blocks);
        println!("  NotRequested: {}", not_requested_blocks);
        println!("  Requested:    {}", requested_blocks);
        println!("  Downloaded:   {}", downloaded_blocks);

        let progress_pct = if total_blocks > 0 {
            (downloaded_blocks as f64 / total_blocks as f64) * 100.0
        } else {
            0.0
        };
        println!("  Progress:     {:.2}%", progress_pct);

        println!("\n--- Heat Map (Top 20) ---");
        let mut heat_entries: Vec<_> = self.heat.iter().collect();
        heat_entries.sort_by_key(|(_, heat)| std::cmp::Reverse(**heat));

        for (request, heat) in heat_entries.iter().take(20) {
            println!(
                "  Piece {} Block @{:6} ({:5} bytes): heat={}",
                request.piece_index, request.begin, request.length, heat
            );
        }

        if heat_entries.len() > 20 {
            println!("  ... and {} more entries", heat_entries.len() - 20);
        }

        println!("\n--- Detailed Piece State ---");
        for piece in &self.pieces {
            // Only print pieces that have activity
            if piece.state == PieceState::NotRequested {
                continue;
            }

            println!(
                "Piece {:4} | State: {:12?} | Peers: {:3} | Blocks: {}/{}",
                piece.index,
                piece.state,
                piece.num_peer,
                piece
                    .blocks
                    .iter()
                    .filter(|b| matches!(b.state, BlockState::Downloaded))
                    .count(),
                piece.blocks.len()
            );

            // Print block details for in-progress pieces
            if piece.state == PieceState::InProgress {
                for (i, block) in piece.blocks.iter().enumerate() {
                    let state_str = match block.state {
                        BlockState::NotRequested => "NotRequested".to_string(),
                        BlockState::Requested { heat } => format!("Requested(heat={})", heat),
                        BlockState::Downloaded => "Downloaded".to_string(),
                    };
                    println!(
                        "  Block {:2} | Offset: {:6} | Length: {:5} | State: {}",
                        i, block.offset, block.length, state_str
                    );
                }
            }
        }

        println!("==============================================\n");
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

    pub fn into_bytes(self) -> Box<[u8]> {
        self.piece_bytes
    }
}
