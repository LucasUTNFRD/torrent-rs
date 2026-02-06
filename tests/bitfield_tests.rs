//! Bitfield integration tests
//!
//! Tests for the bittorrent-core bitfield implementation covering:
//! - Basic set/get operations
//! - Serialization/deserialization
//! - Bitwise operations (AND, OR, etc.)
//! - Edge cases and boundary conditions

use bittorrent_core::bitfield::Bitfield;

#[test]
fn test_bitfield_new() {
    let bf = Bitfield::new(100);
    assert_eq!(bf.len(), 100);
    assert!(!bf.all_set());
    assert!(bf.all_clear());
}

#[test]
fn test_bitfield_set_get() {
    let mut bf = Bitfield::new(100);

    // Initially all bits should be 0
    assert!(!bf.get(0));
    assert!(!bf.get(50));
    assert!(!bf.get(99));

    // Set some bits
    bf.set(0, true);
    bf.set(50, true);
    bf.set(99, true);

    // Verify they're set
    assert!(bf.get(0));
    assert!(bf.get(50));
    assert!(bf.get(99));

    // Other bits should still be 0
    assert!(!bf.get(1));
    assert!(!bf.get(49));
    assert!(!bf.get(51));
}

#[test]
fn test_bitfield_clear() {
    let mut bf = Bitfield::new(100);

    bf.set(50, true);
    assert!(bf.get(50));

    bf.set(50, false);
    assert!(!bf.get(50));
}

#[test]
#[should_panic(expected = "index out of bounds")]
fn test_bitfield_get_out_of_bounds() {
    let bf = Bitfield::new(10);
    let _ = bf.get(10); // Should panic
}

#[test]
#[should_panic(expected = "index out of bounds")]
fn test_bitfield_set_out_of_bounds() {
    let mut bf = Bitfield::new(10);
    bf.set(10, true); // Should panic
}

#[test]
fn test_bitfield_all_set() {
    let mut bf = Bitfield::new(8);

    assert!(!bf.all_set());

    // Set all bits
    for i in 0..8 {
        bf.set(i, true);
    }

    assert!(bf.all_set());
    assert!(!bf.all_clear());
}

#[test]
fn test_bitfield_all_clear() {
    let bf = Bitfield::new(8);
    assert!(bf.all_clear());

    let mut bf = Bitfield::new(8);
    bf.set(0, true);
    assert!(!bf.all_clear());
}

#[test]
fn test_bitfield_count_set() {
    let mut bf = Bitfield::new(100);

    assert_eq!(bf.count_set(), 0);

    bf.set(0, true);
    bf.set(50, true);
    bf.set(99, true);

    assert_eq!(bf.count_set(), 3);
}

#[test]
fn test_bitfield_from_bytes() {
    // 1000 0000 0100 0000 = bits 0 and 9 set
    let bytes = vec![0b1000_0000, 0b0100_0000];
    let bf = Bitfield::from_bytes(bytes, 16);

    assert!(bf.get(0));
    assert!(!bf.get(1));
    assert!(bf.get(9));
    assert!(!bf.get(10));
}

#[test]
fn test_bitfield_to_bytes() {
    let mut bf = Bitfield::new(16);
    bf.set(0, true);
    bf.set(9, true);

    let bytes = bf.to_bytes();
    assert_eq!(bytes.len(), 2);
    assert_eq!(bytes[0], 0b1000_0000);
    assert_eq!(bytes[1], 0b0100_0000);
}

#[test]
fn test_bitfield_set_all() {
    let mut bf = Bitfield::new(100);

    assert!(!bf.all_set());

    bf.set_all();

    assert!(bf.all_set());
    for i in 0..100 {
        assert!(bf.get(i), "Bit {} should be set", i);
    }
}

#[test]
fn test_bitfield_clear_all() {
    let mut bf = Bitfield::new(100);

    bf.set_all();
    assert!(bf.all_set());

    bf.clear_all();

    assert!(bf.all_clear());
    for i in 0..100 {
        assert!(!bf.get(i), "Bit {} should be clear", i);
    }
}

#[test]
fn test_bitfield_and() {
    let mut bf1 = Bitfield::new(16);
    bf1.set(0, true);
    bf1.set(1, true);
    bf1.set(2, true);

    let mut bf2 = Bitfield::new(16);
    bf2.set(1, true);
    bf2.set(2, true);
    bf2.set(3, true);

    let result = bf1.and(&bf2);

    assert!(!result.get(0)); // Only in bf1
    assert!(result.get(1)); // In both
    assert!(result.get(2)); // In both
    assert!(!result.get(3)); // Only in bf2
}

#[test]
fn test_bitfield_or() {
    let mut bf1 = Bitfield::new(16);
    bf1.set(0, true);
    bf1.set(1, true);

    let mut bf2 = Bitfield::new(16);
    bf2.set(1, true);
    bf2.set(2, true);

    let result = bf1.or(&bf2);

    assert!(result.get(0)); // Only in bf1
    assert!(result.get(1)); // In both
    assert!(result.get(2)); // Only in bf2
    assert!(!result.get(3)); // In neither
}

#[test]
fn test_bitfield_xor() {
    let mut bf1 = Bitfield::new(16);
    bf1.set(0, true);
    bf1.set(1, true);

    let mut bf2 = Bitfield::new(16);
    bf2.set(1, true);
    bf2.set(2, true);

    let result = bf1.xor(&bf2);

    assert!(result.get(0)); // Only in bf1
    assert!(!result.get(1)); // In both (cancelled out)
    assert!(result.get(2)); // Only in bf2
    assert!(!result.get(3)); // In neither
}

#[test]
fn test_bitfield_not() {
    let mut bf = Bitfield::new(8);
    bf.set(0, true);
    bf.set(7, true);

    let result = bf.not();

    assert!(!result.get(0));
    assert!(result.get(1));
    assert!(result.get(6));
    assert!(!result.get(7));
}

#[test]
fn test_bitfield_interested_pieces() {
    // We have pieces 0, 1, 2, 5
    let mut our_pieces = Bitfield::new(8);
    our_pieces.set(0, true);
    our_pieces.set(1, true);
    our_pieces.set(2, true);
    our_pieces.set(5, true);

    // Peer has pieces 1, 2, 3, 4
    let mut peer_pieces = Bitfield::new(8);
    peer_pieces.set(1, true);
    peer_pieces.set(2, true);
    peer_pieces.set(3, true);
    peer_pieces.set(4, true);

    // We're interested in pieces peer has that we don't
    let interested = peer_pieces.and(&our_pieces.not());

    assert!(!interested.get(0)); // We have it
    assert!(!interested.get(1)); // We have it
    assert!(!interested.get(2)); // We have it
    assert!(interested.get(3)); // We don't have it, peer does
    assert!(interested.get(4)); // We don't have it, peer does
    assert!(!interested.get(5)); // We have it, peer doesn't
}

#[test]
fn test_bitfield_first_unset() {
    let mut bf = Bitfield::new(100);

    // All clear, first unset should be 0
    assert_eq!(bf.first_unset(), Some(0));

    // Set first 50
    for i in 0..50 {
        bf.set(i, true);
    }

    assert_eq!(bf.first_unset(), Some(50));

    // Set all
    bf.set_all();
    assert_eq!(bf.first_unset(), None);
}

#[test]
fn test_bitfield_next_set() {
    let mut bf = Bitfield::new(100);

    assert_eq!(bf.next_set(0), None);

    bf.set(10, true);
    bf.set(20, true);
    bf.set(30, true);

    assert_eq!(bf.next_set(0), Some(10));
    assert_eq!(bf.next_set(10), Some(10));
    assert_eq!(bf.next_set(11), Some(20));
    assert_eq!(bf.next_set(21), Some(30));
    assert_eq!(bf.next_set(31), None);
}

#[test]
fn test_bitfield_next_unset() {
    let mut bf = Bitfield::new(100);
    bf.set_all();

    assert_eq!(bf.next_unset(0), None);

    bf.set(10, false);
    bf.set(20, false);
    bf.set(30, false);

    assert_eq!(bf.next_unset(0), Some(10));
    assert_eq!(bf.next_unset(10), Some(10));
    assert_eq!(bf.next_unset(11), Some(20));
    assert_eq!(bf.next_unset(21), Some(30));
    assert_eq!(bf.next_unset(31), None);
}

#[test]
fn test_bitfield_empty() {
    let bf = Bitfield::new(0);
    assert_eq!(bf.len(), 0);
    assert!(bf.all_clear());
    assert!(bf.all_set()); // Vacuously true for empty bitfield
}

#[test]
fn test_bitfield_single_bit() {
    let mut bf = Bitfield::new(1);

    assert!(!bf.get(0));

    bf.set(0, true);
    assert!(bf.get(0));
    assert!(bf.all_set());

    let bytes = bf.to_bytes();
    assert_eq!(bytes.len(), 1);
    assert_eq!(bytes[0], 0b1000_0000);
}

#[test]
fn test_bitfield_partial_byte() {
    // 5 bits in one byte
    let mut bf = Bitfield::new(5);
    bf.set(0, true);
    bf.set(4, true);

    let bytes = bf.to_bytes();
    assert_eq!(bytes.len(), 1);
    // Bits are stored MSB first: 1 000 1 000
    assert_eq!(bytes[0], 0b1000_1000);
}

#[test]
fn test_bitfield_clone() {
    let mut bf1 = Bitfield::new(100);
    bf1.set(0, true);
    bf1.set(50, true);
    bf1.set(99, true);

    let bf2 = bf1.clone();

    // Verify cloned bitfield has same values
    assert!(bf2.get(0));
    assert!(bf2.get(50));
    assert!(bf2.get(99));
    assert_eq!(bf2.len(), 100);

    // Modifying one shouldn't affect the other
    bf2.set(1, true);
    assert!(!bf1.get(1));
}

#[test]
fn test_bitfield_equality() {
    let mut bf1 = Bitfield::new(16);
    bf1.set(0, true);
    bf1.set(7, true);
    bf1.set(15, true);

    let mut bf2 = Bitfield::new(16);
    bf2.set(0, true);
    bf2.set(7, true);
    bf2.set(15, true);

    assert_eq!(bf1, bf2);

    bf2.set(1, true);
    assert_ne!(bf1, bf2);
}

#[test]
fn test_bitfield_inequality_different_sizes() {
    let mut bf1 = Bitfield::new(8);
    bf1.set_all();

    let mut bf2 = Bitfield::new(16);
    bf2.set_all();

    assert_ne!(bf1, bf2);
}
