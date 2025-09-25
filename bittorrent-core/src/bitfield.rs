use bytes::Bytes;
use thiserror::Error;

const MSB_MASK: u8 = 0b10000000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BitfieldError {
    #[error("Invalid Length expected{expected_len}, got {actual_len}")]
    InvalidLength {
        expected_len: usize,
        actual_len: usize,
    },
    #[error("Non zero spare bits")]
    NonZeroSpareBits,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Bitfield {
    inner: Vec<u8>,
    num_pieces: usize,
}

impl Bitfield {
    /// Constructs an empty bitfield
    pub fn new() -> Self {
        Self {
            inner: vec![],
            num_pieces: 0,
        }
    }

    /// constructs a bitfield with given size
    pub fn with_size(num_pieces: usize) -> Self {
        let nbytes = (num_pieces).div_ceil(8);
        Self {
            inner: vec![0; nbytes],
            num_pieces,
        }
    }

    pub fn from_bytes_checked(payload: Bytes, num_pieces: usize) -> Result<Self, BitfieldError> {
        let expected_bytes = num_pieces.div_ceil(8);

        if payload.len() < expected_bytes {
            return Err(BitfieldError::InvalidLength {
                expected_len: expected_bytes,
                actual_len: payload.len(),
            });
        }

        // Check spare bits in the last byte
        let last_byte_bits = num_pieces % 8;
        if last_byte_bits != 0 {
            // If num_pieces is not a multiple of 8
            let last_byte = payload[expected_bytes - 1];
            let mask = (1u8 << (8 - last_byte_bits)) - 1; // Mask for spare bits
            if (last_byte & mask) != 0 {
                return Err(BitfieldError::NonZeroSpareBits);
            }
        }

        // Check trailing bytes
        if payload.len() > expected_bytes {
            let extra_bytes = &payload[expected_bytes..];
            if extra_bytes.iter().any(|&b| b != 0) {
                return Err(BitfieldError::NonZeroSpareBits);
            }
        }

        Ok(Self {
            inner: payload.into(),
            num_pieces,
        })
    }

    /// Construct a bitfield from payload without checking internal checking
    /// We MUST have to call check of validate to verify the bitfield was not malformed
    pub fn from_bytes_unchecked(payload: Bytes) -> Self {
        Self {
            inner: payload.into(),
            num_pieces: 0, // on validate method this will be corrected
        }
    }

    /// Validate an unchecked bitfield to the expected num_pieces
    pub fn validate(&mut self, num_pieces: usize) -> Result<(), BitfieldError> {
        let expected_bytes = num_pieces.div_ceil(8);

        if self.inner.len() < expected_bytes {
            return Err(BitfieldError::InvalidLength {
                expected_len: expected_bytes,
                actual_len: self.inner.len(),
            });
        }

        // Check spare bits in the last byte
        let last_byte_bits = num_pieces % 8;
        if last_byte_bits != 0 {
            // If num_pieces is not a multiple of 8
            let last_byte = self.inner[expected_bytes - 1];
            let mask = (1u8 << (8 - last_byte_bits)) - 1; // Mask for spare bits
            if (last_byte & mask) != 0 {
                return Err(BitfieldError::NonZeroSpareBits);
            }
        }

        // Check trailing bytes
        if self.inner.len() > expected_bytes {
            let extra_bytes = &self.inner[expected_bytes..];
            if extra_bytes.iter().any(|&b| b != 0) {
                return Err(BitfieldError::NonZeroSpareBits);
            }
        }

        self.num_pieces = num_pieces;

        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.num_pieces == 0
    }

    pub fn resize(&mut self, needed: usize) {
        let old_size = self.num_pieces;
        let new_bytes = needed.div_ceil(8);

        self.inner.resize(new_bytes, 0);
        self.num_pieces = needed;

        // If we expanded, clear any new bits that might be set
        if needed > old_size {
            self.clear_trailing_bits();
        }
    }

    fn clear_trailing_bits(&mut self) {
        let remainder = self.num_pieces % 8;
        if remainder != 0 && !self.inner.is_empty() {
            let last_idx = (self.num_pieces.div_ceil(8)) - 1;
            if last_idx < self.inner.len() {
                let mask = 0xff << (8 - remainder);
                self.inner[last_idx] &= mask;
            }
        }
    }

    pub fn size(&self) -> usize {
        self.num_pieces
    }

    pub fn set(&mut self, index: usize) {
        let byte_idx = index / 8;
        let bit_idx = index % 8;
        self.inner[byte_idx] |= MSB_MASK >> bit_idx;
    }

    pub fn has(&self, index: usize) -> bool {
        let byte_idx = index / 8;
        let bit_idx = index % 8;
        (self.inner[byte_idx] & (MSB_MASK >> bit_idx)) != 0
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_bitfield_set_and_have() {
        let num_pieces = 64;
        let mut my_bitfield = Bitfield::with_size(num_pieces);

        my_bitfield.set(15);
        assert!(my_bitfield.has(15));
    }

    #[test]
    fn test_bail_of_wrong_length_bitfield() {
        // Test case 1: Bitfield too short
        let num_pieces = 20;
        let payload = Bytes::from(vec![0xFF, 0xFF]); // 2 bytes = 16 bits, but need 20
        let result = Bitfield::from_bytes_checked(payload, num_pieces);
        assert_eq!(
            result,
            Err(BitfieldError::InvalidLength {
                expected_len: 3,
                actual_len: 2
            })
        );

        // Test case 2: Bitfield with non-zero spare bits in last byte
        let num_pieces = 10; // Need 2 bytes, last byte has 6 spare bits
        let payload = Bytes::from(vec![0xFF, 0xFF]); // All bits set including spare bits
        let result = Bitfield::from_bytes_checked(payload, num_pieces);
        assert_eq!(result, Err(BitfieldError::NonZeroSpareBits));

        // Test case 3: Bitfield with correct last byte (spare bits zeroed)
        let num_pieces = 10;
        let payload = Bytes::from(vec![0xFF, 0xC0]); // Last 6 bits are zero
        let result = Bitfield::from_bytes_checked(payload, num_pieces);
        assert!(result.is_ok());
        let bitfield = result.unwrap();
        // Check first 10 bits are set
        for i in 0..10 {
            assert!(bitfield.has(i));
        }

        // Test case 4: Extra trailing bytes with non-zero values
        let num_pieces = 8;
        let payload = Bytes::from(vec![0xFF, 0x01]); // Extra byte with non-zero value
        let result = Bitfield::from_bytes_checked(payload, num_pieces);
        assert_eq!(result, Err(BitfieldError::NonZeroSpareBits));

        // Test case 5: Extra trailing bytes with zero values (should be OK)
        let num_pieces = 8;
        let payload = Bytes::from(vec![0xFF, 0x00, 0x00]); // Extra zero bytes
        let result = Bitfield::from_bytes_checked(payload, num_pieces);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bitfield_from_peer() {
        // Scenario: Receive bitfield from peer before knowing torrent metadata
        let peer_payload = Bytes::from(vec![0b11110000, 0b10100000]); // 16 bits
        let mut bitfield = Bitfield::from_bytes_unchecked(peer_payload);

        // At this point we don't know num_pieces, so bitfield.num_pieces is 0
        assert_eq!(bitfield.num_pieces, 0);

        // Later we learn the torrent has 12 pieces
        let result = bitfield.validate(12);
        assert!(result.is_ok());
        assert_eq!(bitfield.num_pieces, 12);

        // Verify the bits are preserved correctly
        assert!(bitfield.has(0)); // 1
        assert!(bitfield.has(1)); // 1
        assert!(bitfield.has(2)); // 1
        assert!(bitfield.has(3)); // 1
        assert!(!bitfield.has(4)); // 0
        assert!(!bitfield.has(5)); // 0
        assert!(!bitfield.has(6)); // 0
        assert!(!bitfield.has(7)); // 0
        assert!(bitfield.has(8)); // 1
        assert!(!bitfield.has(9)); // 0
        assert!(bitfield.has(10)); // 1
        assert!(!bitfield.has(11)); // 0

        // Test validation failure: spare bits set
        let bad_payload = Bytes::from(vec![0xFF, 0xFF]); // All bits set
        let mut bad_bitfield = Bitfield::from_bytes_unchecked(bad_payload);
        let result = bad_bitfield.validate(10); // 10 pieces means last 6 bits should be 0
        assert_eq!(result, Err(BitfieldError::NonZeroSpareBits));

        // Test validation failure: too short
        let short_payload = Bytes::from(vec![0xFF]);
        let mut short_bitfield = Bitfield::from_bytes_unchecked(short_payload);
        let result = short_bitfield.validate(20); // Need at least 3 bytes
        assert_eq!(
            result,
            Err(BitfieldError::InvalidLength {
                expected_len: 3,
                actual_len: 1
            })
        );
    }

    #[test]
    fn test_bitfield_operations() {
        let mut bitfield = Bitfield::with_size(20);

        // Test initial state - all bits should be 0
        for i in 0..20 {
            assert!(!bitfield.has(i));
        }

        // Set specific bits
        bitfield.set(0);
        bitfield.set(7);
        bitfield.set(8);
        bitfield.set(19);

        assert!(bitfield.has(0));
        assert!(bitfield.has(7));
        assert!(bitfield.has(8));
        assert!(bitfield.has(19));
        assert!(!bitfield.has(1));
        assert!(!bitfield.has(18));

        // Verify internal representation
        assert_eq!(bitfield.inner[0], 0b10000001); // bits 0 and 7
        assert_eq!(bitfield.inner[1], 0b10000000); // bit 8
        assert_eq!(bitfield.inner[2], 0b00010000); // bit 19
    }

    #[test]
    fn test_resize() {
        let mut bitfield = Bitfield::with_size(8);
        bitfield.set(0);
        bitfield.set(7);

        // Resize to larger
        bitfield.resize(16);
        assert_eq!(bitfield.size(), 16);
        assert!(bitfield.has(0));
        assert!(bitfield.has(7));
        assert!(!bitfield.has(8));
        assert!(!bitfield.has(15));

        // Set bit in new range
        bitfield.set(15);
        assert!(bitfield.has(15));

        // Resize to smaller
        bitfield.resize(4);
        assert_eq!(bitfield.size(), 4);
        assert!(bitfield.has(0));
        assert!(!bitfield.has(7)); // This will panic in real usage but test the resize logic
    }

    #[test]
    fn test_lazy_bitfield_scenario() {
        // Simulate Deluge client sending incomplete bitfield initially
        let num_pieces = 16;
        let initial_payload = Bytes::from(vec![0b11110000, 0b10100000]); // Missing some pieces
        let bitfield = Bitfield::from_bytes_checked(initial_payload, num_pieces);
        assert!(bitfield.is_ok());

        let bf = bitfield.unwrap();
        // Client has pieces 0,1,2,3,8,10 but not others
        assert!(bf.has(0));
        assert!(bf.has(1));
        assert!(bf.has(2));
        assert!(bf.has(3));
        assert!(!bf.has(4));
        assert!(bf.has(8));
        assert!(!bf.has(9));
        assert!(bf.has(10));
        // Later client would send HAVE messages for remaining pieces
    }

    #[test]
    fn test_edge_cases() {
        // Empty bitfield
        let bitfield = Bitfield::new();
        assert!(bitfield.is_empty());
        assert_eq!(bitfield.size(), 0);

        // Single bit
        let mut single = Bitfield::with_size(1);
        assert!(!single.has(0));
        single.set(0);
        assert!(single.has(0));
        assert_eq!(single.inner[0], 0b10000000);

        // Exactly 8 bits
        let eight = Bitfield::with_size(8);
        assert_eq!(eight.inner.len(), 1);

        // 9 bits requires 2 bytes
        let nine = Bitfield::with_size(9);
        assert_eq!(nine.inner.len(), 2);
    }

    #[test]
    fn test_clear_trailing_bits() {
        let mut bitfield = Bitfield::with_size(10);

        // Manually corrupt the trailing bits
        bitfield.inner[1] = 0xFF; // Set all bits in second byte

        // Clear trailing should fix this
        bitfield.clear_trailing_bits();

        // Last 6 bits of second byte should be 0
        assert_eq!(bitfield.inner[1] & 0b00111111, 0);
    }

    #[test]
    fn test_bitfield_from_all_pieces() {
        // Simulate a seeder with all pieces
        let num_pieces = 23;
        let mut payload = vec![0xFF, 0xFF, 0xFF]; // All bits set

        // Clear the spare bits in the last byte
        let spare_bits = 8 - (num_pieces % 8);
        if spare_bits < 8 {
            let last_idx = payload.len() - 1;
            payload[last_idx] = 0xFF << spare_bits;
        }

        let bitfield = Bitfield::from_bytes_checked(Bytes::from(payload), num_pieces);
        assert!(bitfield.is_ok());

        let bf = bitfield.unwrap();
        for i in 0..num_pieces {
            assert!(bf.has(i), "Piece {} should be set", i);
        }
    }
}
