use std::sync::Arc;

use bittorrent_common::metainfo::TorrentInfo;
use peer_protocol::protocol::BlockInfo;

use super::manager::bitfield::Bitfield;

pub const BLOCK_SIZE: u32 = 1 << 14;

// reference: https://blog.libtorrent.org/2011/11/writing-a-fast-piece-picker/

#[allow(dead_code)]
pub struct Picker {
    torrent: Arc<TorrentInfo>,
    num_pieces: usize,
    piece_availability: Vec<PieceIndex>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PieceIndex {
    availability: usize,
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
                availability: 0,
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

    pub fn remaining(&self) -> usize {
        self.piece_availability
            .iter()
            .filter(|p| p.state != PieceState::Finished)
            .count()
    }

    pub fn increment_availability(&mut self, update: AvailabilityUpdate) {
        match update {
            AvailabilityUpdate::Bitfield(bitfield) => {
                if bitfield.all_set() {
                    self.piece_availability
                        .iter_mut()
                        .for_each(|p| p.availability += 1);
                } else {
                    for idx in bitfield.iter_set() {
                        self.piece_availability[idx].availability += 1;
                    }
                }
            }
            AvailabilityUpdate::Index(idx) => {
                if let Some(p) = self.piece_availability.get_mut(idx as usize) {
                    p.availability += 1;
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
                        .for_each(|p| p.availability = p.availability.saturating_sub(1));
                } else {
                    for idx in bitfield.iter_set() {
                        let p = &mut self.piece_availability[idx];
                        p.availability = p.availability.saturating_sub(1);
                    }
                }
            }
            AvailabilityUpdate::Index(idx) => {
                if let Some(p) = self.piece_availability.get_mut(idx as usize) {
                    p.availability = p.availability.saturating_sub(1)
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
            .min_by_key(|&i| self.piece_availability[i].availability)?;

        // let piece_idx = candidate.first()?;
        let blocks = self.get_blocks(piece_idx);

        debug_assert_eq!(self.piece_availability[piece_idx].state, PieceState::None);

        Some((piece_idx, blocks))
    }

    pub fn mark_piece_as(&mut self, piece_idx: usize, state: PieceState) {
        if let Some(piece) = self.piece_availability.get_mut(piece_idx) {
            piece.state = state;
        }
    }

    pub fn summary(&self) -> (usize, usize, usize, usize) {
        let mut none = 0;
        let mut requested = 0;
        let mut writing = 0;
        let mut finished = 0;

        for p in &self.piece_availability {
            match p.state {
                PieceState::None => none += 1,
                PieceState::Requested => requested += 1,
                PieceState::Writing => writing += 1,
                PieceState::Finished => finished += 1,
            }
        }

        (none, requested, writing, finished)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use bittorrent_common::metainfo::{FileMode, Info, TorrentInfo};
    use bittorrent_common::types::InfoHash;

    /// Creates a mock TorrentInfo for testing
    fn create_mock_torrent(num_pieces: usize, piece_length: u32) -> Arc<TorrentInfo> {
        let pieces: Vec<[u8; 20]> = (0..num_pieces)
            .map(|i| {
                let mut piece_hash = [0u8; 20];
                piece_hash[0] = i as u8;
                piece_hash
            })
            .collect();

        let info = Info {
            piece_length: piece_length as i64,
            pieces,
            private: None,
            mode: FileMode::SingleFile {
                name: "test.txt".to_string(),
                length: (num_pieces as u32 * piece_length) as i64,
                md5sum: None,
            },
        };

        Arc::new(TorrentInfo {
            info,
            announce: "http://tracker.example.com/announce".to_string(),
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new([0u8; 20]),
        })
    }

    /// Creates a bitfield with specific pieces set
    fn create_bitfield(num_pieces: usize, set_pieces: &[usize]) -> Bitfield {
        let mut bitfield = Bitfield::new(num_pieces);
        for &piece_idx in set_pieces {
            bitfield.set(piece_idx).unwrap();
        }
        bitfield
    }

    #[test]
    fn test_picker_initialization() {
        let torrent = create_mock_torrent(10, 262144); // 256KB pieces
        let picker = Picker::new(torrent.clone());

        assert_eq!(picker.num_pieces, 10);
        assert_eq!(picker.piece_availability.len(), 10);

        // All pieces should start with zero availability and None state
        for piece in &picker.piece_availability {
            assert_eq!(piece.availability, 0);
            assert_eq!(piece.state, PieceState::None);
            assert!(!piece.partial);
        }

        // Check piece sizes
        for i in 0..9 {
            assert_eq!(picker.piece_availability[i].size, 262144);
        }
        // Last piece might be smaller
        assert!(picker.piece_availability[9].size > 0);
    }

    #[test]
    fn test_increment_availability_bitfield() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Create bitfield with pieces 0, 2, 4 set
        let bitfield = create_bitfield(5, &[0, 2, 4]);
        picker.increment_availability(AvailabilityUpdate::Bitfield(&bitfield));

        assert_eq!(picker.piece_availability[0].availability, 1);
        assert_eq!(picker.piece_availability[1].availability, 0);
        assert_eq!(picker.piece_availability[2].availability, 1);
        assert_eq!(picker.piece_availability[3].availability, 0);
        assert_eq!(picker.piece_availability[4].availability, 1);
    }

    #[test]
    fn test_increment_availability_all_set() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Create bitfield with all pieces set
        let mut bitfield = Bitfield::new(5);
        for i in 0..5 {
            bitfield.set(i).unwrap();
        }

        picker.increment_availability(AvailabilityUpdate::Bitfield(&bitfield));

        // All pieces should have availability 1
        for piece in &picker.piece_availability {
            assert_eq!(piece.availability, 1);
        }
    }

    #[test]
    fn test_increment_availability_index() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        picker.increment_availability(AvailabilityUpdate::Index(2));
        picker.increment_availability(AvailabilityUpdate::Index(2));
        picker.increment_availability(AvailabilityUpdate::Index(4));

        assert_eq!(picker.piece_availability[0].availability, 0);
        assert_eq!(picker.piece_availability[1].availability, 0);
        assert_eq!(picker.piece_availability[2].availability, 2);
        assert_eq!(picker.piece_availability[3].availability, 0);
        assert_eq!(picker.piece_availability[4].availability, 1);
    }

    #[test]
    fn test_increment_availability_out_of_bounds() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Should not panic for out-of-bounds index
        picker.increment_availability(AvailabilityUpdate::Index(10));

        // All pieces should still have zero availability
        for piece in &picker.piece_availability {
            assert_eq!(piece.availability, 0);
        }
    }

    #[test]
    fn test_decrement_availability_bitfield() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // First increment availability
        let bitfield = create_bitfield(5, &[0, 2, 4]);
        picker.increment_availability(AvailabilityUpdate::Bitfield(&bitfield));
        picker.increment_availability(AvailabilityUpdate::Bitfield(&bitfield));

        // All set pieces should have availability 2
        assert_eq!(picker.piece_availability[0].availability, 2);
        assert_eq!(picker.piece_availability[2].availability, 2);
        assert_eq!(picker.piece_availability[4].availability, 2);

        // Now decrement
        picker.decrement_availability(AvailabilityUpdate::Bitfield(&bitfield));

        assert_eq!(picker.piece_availability[0].availability, 1);
        assert_eq!(picker.piece_availability[1].availability, 0);
        assert_eq!(picker.piece_availability[2].availability, 1);
        assert_eq!(picker.piece_availability[3].availability, 0);
        assert_eq!(picker.piece_availability[4].availability, 1);
    }

    #[test]
    fn test_decrement_availability_saturating() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Decrement when availability is already 0 (should saturate at 0)
        picker.decrement_availability(AvailabilityUpdate::Index(2));

        assert_eq!(picker.piece_availability[2].availability, 0);
    }

    #[test]
    fn test_send_interest_bitfield() {
        let torrent = create_mock_torrent(5, 16384);
        let picker = Picker::new(torrent);

        let bitfield = create_bitfield(5, &[0, 2, 4]);

        // Should be interested since we don't have any pieces (all state == None)
        assert!(picker.send_interest(AvailabilityUpdate::Bitfield(&bitfield)));

        // Empty bitfield should not generate interest
        let empty_bitfield = Bitfield::new(5);
        assert!(!picker.send_interest(AvailabilityUpdate::Bitfield(&empty_bitfield)));
    }

    #[test]
    fn test_send_interest_finished_pieces() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Mark some pieces as finished
        picker.mark_piece_as(0, PieceState::Finished);
        picker.mark_piece_as(2, PieceState::Finished);

        let bitfield = create_bitfield(5, &[0, 2]); // Only finished pieces

        // Should not be interested since we already have these pieces
        assert!(!picker.send_interest(AvailabilityUpdate::Bitfield(&bitfield)));

        let bitfield_with_new = create_bitfield(5, &[0, 1, 2]); // Includes piece 1

        // Should be interested because of piece 1
        assert!(picker.send_interest(AvailabilityUpdate::Bitfield(&bitfield_with_new)));
    }

    #[test]
    fn test_send_interest_index() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Should be interested in piece 3 (state == None)
        assert!(picker.send_interest(AvailabilityUpdate::Index(3)));

        // Mark piece 3 as finished
        picker.mark_piece_as(3, PieceState::Finished);

        // Should not be interested anymore
        assert!(!picker.send_interest(AvailabilityUpdate::Index(3)));
    }

    #[test]
    fn test_pick_piece_rarest_first() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Set different availabilities
        picker.increment_availability(AvailabilityUpdate::Index(0)); // availability: 1
        picker.increment_availability(AvailabilityUpdate::Index(1)); // availability: 1
        picker.increment_availability(AvailabilityUpdate::Index(1)); // availability: 2
        picker.increment_availability(AvailabilityUpdate::Index(2)); // availability: 1
        picker.increment_availability(AvailabilityUpdate::Index(2)); // availability: 2
        picker.increment_availability(AvailabilityUpdate::Index(2)); // availability: 3

        // Peer has pieces 0, 1, 2
        let peer_bitfield = create_bitfield(5, &[0, 1, 2]);

        // Should pick piece 0 (rarest among available pieces)
        let result = picker.pick_piece(&peer_bitfield);
        assert!(result.is_some());
        let (piece_idx, blocks) = result.unwrap();
        assert_eq!(piece_idx, 0);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn test_pick_piece_no_available_pieces() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Mark all pieces as finished
        for i in 0..5 {
            picker.mark_piece_as(i, PieceState::Finished);
        }

        let peer_bitfield = create_bitfield(5, &[0, 1, 2, 3, 4]);
        let result = picker.pick_piece(&peer_bitfield);

        // Should return None since we have all pieces
        assert!(result.is_none());
    }

    #[test]
    fn test_pick_piece_peer_has_no_pieces_we_need() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Mark pieces 0, 1 as finished
        picker.mark_piece_as(0, PieceState::Finished);
        picker.mark_piece_as(1, PieceState::Finished);

        // Peer only has pieces we already have
        let peer_bitfield = create_bitfield(5, &[0, 1]);
        let result = picker.pick_piece(&peer_bitfield);

        // Should return None
        assert!(result.is_none());
    }

    #[test]
    fn test_get_blocks_calculation() {
        let torrent = create_mock_torrent(5, 40000); // 40KB pieces
        let picker = Picker::new(torrent);

        let blocks = picker.get_blocks(0);

        // 40KB piece should be split into 3 blocks:
        // Block 0: 16KB (16384 bytes)
        // Block 1: 16KB (16384 bytes)
        // Block 2: ~8KB (7232 bytes)
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].index, 0);
        assert_eq!(blocks[0].begin, 0);
        assert_eq!(blocks[0].length, BLOCK_SIZE);

        assert_eq!(blocks[1].index, 0);
        assert_eq!(blocks[1].begin, BLOCK_SIZE);
        assert_eq!(blocks[1].length, BLOCK_SIZE);

        assert_eq!(blocks[2].index, 0);
        assert_eq!(blocks[2].begin, 2 * BLOCK_SIZE);
        assert_eq!(blocks[2].length, 40000 - 2 * BLOCK_SIZE);
    }

    #[test]
    fn test_get_blocks_small_piece() {
        let torrent = create_mock_torrent(5, 8192); // 8KB pieces (smaller than one block)
        let picker = Picker::new(torrent);

        let blocks = picker.get_blocks(0);

        // Small piece should result in a single block
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].index, 0);
        assert_eq!(blocks[0].begin, 0);
        assert_eq!(blocks[0].length, 8192);
    }

    #[test]
    fn test_mark_piece_as() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Initially all pieces should be in None state
        assert_eq!(picker.piece_availability[2].state, PieceState::None);

        picker.mark_piece_as(2, PieceState::Requested);
        assert_eq!(picker.piece_availability[2].state, PieceState::Requested);

        picker.mark_piece_as(2, PieceState::Writing);
        assert_eq!(picker.piece_availability[2].state, PieceState::Writing);

        picker.mark_piece_as(2, PieceState::Finished);
        assert_eq!(picker.piece_availability[2].state, PieceState::Finished);
    }

    #[test]
    fn test_mark_piece_as_out_of_bounds() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Should not panic for out-of-bounds index
        picker.mark_piece_as(10, PieceState::Finished);

        // All pieces should still be in None state
        for piece in &picker.piece_availability {
            assert_eq!(piece.state, PieceState::None);
        }
    }

    #[test]
    fn test_picker_workflow() {
        let torrent = create_mock_torrent(5, 16384);
        let mut picker = Picker::new(torrent);

        // Simulate peer connecting with some pieces
        let peer_bitfield = create_bitfield(5, &[1, 2, 3]);
        picker.increment_availability(AvailabilityUpdate::Bitfield(&peer_bitfield));

        // Check if we should be interested (we should, since we don't have any pieces)
        assert!(picker.send_interest(AvailabilityUpdate::Bitfield(&peer_bitfield)));

        // Pick a piece to download
        let result = picker.pick_piece(&peer_bitfield);
        assert!(result.is_some());
        let (piece_idx, _blocks) = result.unwrap();
        assert!(peer_bitfield.has(piece_idx));

        // Mark piece as being downloaded
        picker.mark_piece_as(piece_idx, PieceState::Requested);

        // Trying to pick again should not return the same piece
        let result2 = picker.pick_piece(&peer_bitfield);
        if let Some((piece_idx2, _)) = result2 {
            assert_ne!(piece_idx, piece_idx2);
        }

        // Complete the download
        picker.mark_piece_as(piece_idx, PieceState::Finished);

        // Peer disconnects
        picker.decrement_availability(AvailabilityUpdate::Bitfield(&peer_bitfield));

        // Availability should be decremented for remaining pieces
        for i in 0..5 {
            if i == piece_idx {
                // Finished piece still has state Finished
                assert_eq!(picker.piece_availability[i].state, PieceState::Finished);
            } else if peer_bitfield.has(i) {
                // Other pieces from peer should have availability 0 now
                assert_eq!(picker.piece_availability[i].availability, 0);
            }
        }
    }

    #[test]
    fn test_multiple_peers_same_pieces() {
        let torrent = create_mock_torrent(3, 16384);
        let mut picker = Picker::new(torrent);

        // Two peers with overlapping pieces
        let peer1_bitfield = create_bitfield(3, &[0, 1]);
        let peer2_bitfield = create_bitfield(3, &[1, 2]);

        picker.increment_availability(AvailabilityUpdate::Bitfield(&peer1_bitfield));
        picker.increment_availability(AvailabilityUpdate::Bitfield(&peer2_bitfield));

        // Piece 1 should have availability 2, others should have availability 1
        assert_eq!(picker.piece_availability[0].availability, 1);
        assert_eq!(picker.piece_availability[1].availability, 2);
        assert_eq!(picker.piece_availability[2].availability, 1);

        // When picking from peer1, should prefer piece 0 (rarest)
        let result = picker.pick_piece(&peer1_bitfield);
        assert!(result.is_some());
        let (piece_idx, _) = result.unwrap();
        assert_eq!(piece_idx, 0); // Piece 0 is rarer than piece 1
    }

    #[test]
    fn test_edge_case_empty_torrent() {
        let torrent = create_mock_torrent(0, 16384);
        let picker = Picker::new(torrent);

        assert_eq!(picker.num_pieces, 0);
        assert_eq!(picker.piece_availability.len(), 0);

        let empty_bitfield = Bitfield::new(0);
        let result = picker.pick_piece(&empty_bitfield);
        assert!(result.is_none());

        let (none, requested, writing, finished) = picker.summary();
        assert_eq!(none, 0);
        assert_eq!(requested, 0);
        assert_eq!(writing, 0);
        assert_eq!(finished, 0);
    }

    #[test]
    fn test_piece_state_transitions() {
        let torrent = create_mock_torrent(1, 16384);
        let mut picker = Picker::new(torrent);

        // Test all valid state transitions
        assert_eq!(picker.piece_availability[0].state, PieceState::None);

        picker.mark_piece_as(0, PieceState::Requested);
        assert_eq!(picker.piece_availability[0].state, PieceState::Requested);

        picker.mark_piece_as(0, PieceState::Writing);
        assert_eq!(picker.piece_availability[0].state, PieceState::Writing);

        picker.mark_piece_as(0, PieceState::Finished);
        assert_eq!(picker.piece_availability[0].state, PieceState::Finished);

        // Test that we can go back to None (in case of error/retry)
        picker.mark_piece_as(0, PieceState::None);
        assert_eq!(picker.piece_availability[0].state, PieceState::None);
    }
}
