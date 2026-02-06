//! Bitfield integration tests
//!
//! Tests for the bittorrent-core bitfield implementation covering:
//! - Basic set/get operations
//! - Serialization/deserialization
//! - Edge cases and boundary conditions

use bittorrent_core::Bitfield;
use bytes::Bytes;

#[test]
fn test_bitfield_with_size() {
    let bf = Bitfield::with_size(100);
    assert_eq!(bf.size(), 100);
    assert!(bf.is_empty() == false);
}

#[test]
fn test_bitfield_new_empty() {
    let bf = Bitfield::new();
    assert_eq!(bf.size(), 0);
    assert!(bf.is_empty());
}

#[test]
fn test_bitfield_set_has() {
    let mut bf = Bitfield::with_size(100);

    // Initially all bits should be 0
    assert!(!bf.has(0));
    assert!(!bf.has(50));
    assert!(!bf.has(99));

    // Set some bits
    bf.set(0);
    bf.set(50);
    bf.set(99);

    // Verify they're set
    assert!(bf.has(0));
    assert!(bf.has(50));
    assert!(bf.has(99));

    // Other bits should still be 0
    assert!(!bf.has(1));
    assert!(!bf.has(49));
    assert!(!bf.has(51));
}

#[test]
fn test_bitfield_from_bytes_checked() {
    // 1000 0000 0100 0000 = bits 0 and 9 set
    let bytes = Bytes::from(vec![0b1000_0000, 0b0100_0000]);
    let bf = Bitfield::from_bytes_checked(bytes, 16).unwrap();

    assert!(bf.has(0));
    assert!(!bf.has(1));
    assert!(bf.has(9));
    assert!(!bf.has(10));
}

#[test]
fn test_bitfield_from_bytes_unchecked_and_validate() {
    // Receive bitfield from peer before knowing torrent metadata
    let peer_payload = Bytes::from(vec![0b11110000, 0b10100000]); // 16 bits
    let mut bitfield = Bitfield::from_bytes_unchecked(peer_payload);

    // At this point we don't know num_pieces
    assert_eq!(bitfield.size(), 0);

    // Later we learn the torrent has 12 pieces
    let result = bitfield.validate(12);
    assert!(result.is_ok());
    assert_eq!(bitfield.size(), 12);

    // Verify the bits are preserved correctly
    assert!(bitfield.has(0));
    assert!(bitfield.has(1));
    assert!(bitfield.has(2));
    assert!(bitfield.has(3));
    assert!(!bitfield.has(4));
    assert!(!bitfield.has(7));
    assert!(bitfield.has(8));
    assert!(!bitfield.has(9));
    assert!(bitfield.has(10));
    assert!(!bitfield.has(11));
}

#[test]
fn test_bitfield_resize() {
    let mut bf = Bitfield::with_size(8);
    bf.set(0);
    bf.set(7);

    // Resize to larger
    bf.resize(16);
    assert_eq!(bf.size(), 16);
    assert!(bf.has(0));
    assert!(bf.has(7));
    assert!(!bf.has(8));
    assert!(!bf.has(15));

    // Set bit in new range
    bf.set(15);
    assert!(bf.has(15));

    // Resize to smaller
    bf.resize(4);
    assert_eq!(bf.size(), 4);
    assert!(bf.has(0));
}

#[test]
fn test_bitfield_iter_set() {
    let mut bf = Bitfield::with_size(20);

    // Set specific bits
    bf.set(0);
    bf.set(7);
    bf.set(8);
    bf.set(19);

    let set_pieces: Vec<usize> = bf.iter_set().collect();
    assert_eq!(set_pieces, vec![0, 7, 8, 19]);
}

#[test]
fn test_bitfield_error_invalid_length() {
    let num_pieces = 20;
    let payload = Bytes::from(vec![0xFF, 0xFF]); // 2 bytes = 16 bits, but need 20 (3 bytes)
    let result = Bitfield::from_bytes_checked(payload, num_pieces);
    assert!(result.is_err());
}

#[test]
fn test_bitfield_error_non_zero_spare_bits() {
    // Bitfield with non-zero spare bits in last byte
    let num_pieces = 10; // Need 2 bytes, last byte has 6 spare bits
    let payload = Bytes::from(vec![0xFF, 0xFF]); // All bits set including spare bits
    let result = Bitfield::from_bytes_checked(payload, num_pieces);
    assert!(result.is_err());
}

#[test]
fn test_bitfield_single_bit() {
    let mut bf = Bitfield::with_size(1);
    assert!(!bf.has(0));

    bf.set(0);
    assert!(bf.has(0));
}

#[test]
fn test_bitfield_edge_cases() {
    // Empty bitfield
    let bf = Bitfield::new();
    assert!(bf.is_empty());
    assert_eq!(bf.size(), 0);

    // Exactly 8 bits
    let eight = Bitfield::with_size(8);
    assert_eq!(eight.size(), 8);

    // 9 bits requires 2 bytes
    let nine = Bitfield::with_size(9);
    assert_eq!(nine.size(), 9);
}
