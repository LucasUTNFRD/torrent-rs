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
    #[error("Index {idx} out of bounds (len {len})")]
    OutOfBounds { idx: usize, len: usize },
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
        let nbytes = (num_pieces + 7).div_ceil(8);
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

    pub fn set_bit(&mut self, index: usize) {
        let byte_idx = index / 8;
        let bit_idx = index % 8;
        self.inner[byte_idx] |= MSB_MASK >> bit_idx;
    }

    pub fn has_bit(&self, index: usize) -> bool {
        let byte_idx = index / 8;
        let bit_idx = index % 8;
        (self.inner[byte_idx] & (MSB_MASK >> bit_idx)) != 0
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn name() {
        todo!();
    }
}
