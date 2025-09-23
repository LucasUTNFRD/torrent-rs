use std::u16;

use bytes::Bytes;
use thiserror::Error;

// Fixed size → so we don’t need Vec, just a boxed slice.
// Mutation only by a manager → internal mutability isn’t needed if you keep it behind &mut or an owner.
// Shared as snapshot → when handed out, others shouldn’t observe future mutations.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Bitfield {
    bits: Box<[u8]>,
    nbits: usize,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BitfieldError {
    #[error("Invalid Length expected{expected_len}, got {actual_len}")]
    InvalidLength {
        expected_len: usize,
        actual_len: usize,
    },
    #[error("Non zero spare bits")]
    NonZeroSpareBits,
    #[error("Index {idx} out of bounds (len {len})")]
    OutOfBounds { idx: usize, len: usize },
}

impl TryFrom<(Bytes, usize)> for Bitfield {
    type Error = BitfieldError;

    fn try_from((bytes, num_pieces): (Bytes, usize)) -> Result<Self, Self::Error> {
        let expected_bytes = num_pieces.div_ceil(8);

        if bytes.len() < expected_bytes {
            return Err(BitfieldError::InvalidLength {
                expected_len: expected_bytes,
                actual_len: bytes.len(),
            });
        }

        // Check spare bits in the last byte
        let last_byte_bits = num_pieces % 8;
        if last_byte_bits != 0 {
            // If num_pieces is not a multiple of 8
            let last_byte = bytes[expected_bytes - 1];
            let mask = (1u8 << (8 - last_byte_bits)) - 1; // Mask for spare bits
            if (last_byte & mask) != 0 {
                return Err(BitfieldError::NonZeroSpareBits);
            }
        }

        // Check trailing bytes
        if bytes.len() > expected_bytes {
            let extra_bytes = &bytes[expected_bytes..];
            if extra_bytes.iter().any(|&b| b != 0) {
                return Err(BitfieldError::NonZeroSpareBits);
            }
        }

        Ok(Self {
            bits: Box::from(&bytes[..]),
            nbits: num_pieces,
        })
    }
}

impl Bitfield {
    pub fn new(nbits: usize) -> Self {
        let nbytes = (nbits + 7).div_ceil(8);
        Self {
            bits: vec![0; nbytes].into_boxed_slice(),
            nbits,
        }
    }

    // TODO: uninitialized bitfield

    pub fn as_bytes(&self) -> Bytes {
        Bytes::from(self.bits.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|b| *b == 0)
    }

    pub fn all_set(&self) -> bool {
        if self.nbits == 0 {
            return false;
        }

        let full_words = self.nbits / 32;
        let remaining_bits = self.nbits % 32;

        // Check full 32-bit words
        for i in 0..full_words {
            let idx = i * 4;
            let word = u32::from_be_bytes(self.bits[idx..idx + 4].try_into().unwrap());
            if word != 0xFFFF_FFFF {
                return false;
            }
        }

        // Check leftover bits in last partial word
        if remaining_bits > 0 {
            let idx = full_words * 4;
            let mut last_word_bytes = [0u8; 4];
            for (j, byte) in self.bits[idx..].iter().take(4).enumerate() {
                last_word_bytes[j] = *byte;
            }
            let word = u32::from_be_bytes(last_word_bytes);
            let mask = 0xFFFF_FFFF_u32 << (32 - remaining_bits);
            if word & mask != mask {
                return false;
            }
        }

        true
    }

    pub fn has(&self, index: usize) -> bool {
        if index >= self.nbits {
            return false;
        }
        let byte_index = index / 8;
        let bit_index = 7 - (index % 8);
        (self.bits[byte_index] >> bit_index) & 1 != 0
    }

    pub fn set(&mut self, index: usize) -> Result<(), BitfieldError> {
        if index >= self.nbits {
            return Err(BitfieldError::OutOfBounds {
                idx: index,
                len: self.nbits,
            });
        }
        let byte_index = index / 8;
        let bit_index = 7 - (index % 8);
        self.bits[byte_index] |= 1 << bit_index;
        Ok(())
    }

    pub fn iter_set(&self) -> BitfieldSetIter<'_> {
        BitfieldSetIter {
            bitfield: self,
            byte_idx: 0,
            bit_in_byte: 0,
        }
    }
}

pub struct BitfieldSetIter<'a> {
    bitfield: &'a Bitfield,
    byte_idx: usize,
    bit_in_byte: u8,
}

impl Iterator for BitfieldSetIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        while self.byte_idx < self.bitfield.bits.len() {
            let byte = self.bitfield.bits[self.byte_idx];
            while self.bit_in_byte < 8 {
                let mask = 1u8 << (7 - self.bit_in_byte);
                let idx = self.byte_idx * 8 + self.bit_in_byte as usize;
                self.bit_in_byte += 1;

                // Skip spare bits beyond nbits
                if idx >= self.bitfield.nbits {
                    continue;
                }

                if (byte & mask) != 0 {
                    return Some(idx);
                }
            }
            self.byte_idx += 1;
            self.bit_in_byte = 0;
        }
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_all_set() {
        // Empty bitfield
        let bf = Bitfield::new(0);
        assert!(!bf.all_set());

        // Single bit, unset
        let mut bf = Bitfield::new(1);
        assert!(!bf.all_set());

        // Single bit, set
        bf.set(0).unwrap();
        assert!(bf.all_set());

        // Multiple bits, all set
        let mut bf = Bitfield::new(10);
        for i in 0..10 {
            bf.set(i).unwrap();
        }
        assert!(bf.all_set());

        // Multiple bits, one unset
        let mut bf = Bitfield::new(10);
        for i in 0..9 {
            bf.set(i).unwrap();
        }
        assert!(!bf.all_set());

        // Edge case: last partial byte
        let mut bf = Bitfield::new(14);
        for i in 0..14 {
            bf.set(i).unwrap();
        }
        assert!(bf.all_set());

        // Edge case: last partial byte, one bit unset
        let mut bf = Bitfield::new(14);
        for i in 0..13 {
            bf.set(i).unwrap();
        }
        assert!(!bf.all_set());
    }

    #[test]
    fn test_set_iter() {
        // Create a bitfield with 10 bits
        let mut bf = Bitfield::new(10);

        // No bits set yet
        let bits: Vec<usize> = bf.iter_set().collect();
        assert!(bits.is_empty());

        // Set some bits
        bf.set(0).unwrap();
        bf.set(3).unwrap();
        bf.set(9).unwrap();

        let bits: Vec<usize> = bf.iter_set().collect();
        assert_eq!(bits, vec![0, 3, 9]);

        // Set all bits
        for i in 0..10 {
            bf.set(i).unwrap();
        }
        let bits: Vec<usize> = bf.iter_set().collect();
        assert_eq!(bits, (0..10).collect::<Vec<_>>());
    }
}
