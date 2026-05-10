use std::array;
use bencode::{Encode, Decode, Bencode, BencodeError};

pub const NODE_ID_LEN: usize = 20;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct NodeId([u8; NODE_ID_LEN]);

impl Encode for NodeId {
    fn to_bencode(&self) -> Bencode {
        Bencode::Bytes(self.0.to_vec())
    }
}

impl Decode for NodeId {
    fn from_bencode(bencode: &Bencode) -> Result<Self, BencodeError> {
        match bencode {
            Bencode::Bytes(b) if b.len() == NODE_ID_LEN => {
                let mut arr = [0u8; NODE_ID_LEN];
                arr.copy_from_slice(b);
                Ok(Self(arr))
            }
            _ => Err(BencodeError::InvalidBencodeString),
        }
    }
}

impl NodeId {
    pub fn from_bytes(bytes: [u8; NODE_ID_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; NODE_ID_LEN] {
        &self.0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Returns the "distance exponent" (159 - leading_zeros)
    /// Used to determine bucket index.
    pub fn distance_exp(&self, other: &Self) -> usize {
        let distance = self ^ other;
        let lz = distance.leading_zeros();
        if lz >= 160 { 0 } else { 159 - lz }
    }

    /// Count leading zero bits in the ID
    fn leading_zeros(&self) -> usize {
        let mut count = 0;
        for &byte in &self.0 {
            if byte == 0 {
                count += 8;
            } else {
                count += byte.leading_zeros() as usize;
                break;
            }
        }
        count
    }
}

impl std::ops::BitXor for NodeId {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self::Output {
        Self(array::from_fn(|i| self.0[i] ^ rhs.0[i]))
    }
}

// Implementation for borrowed values
impl std::ops::BitXor<&NodeId> for &NodeId {
    type Output = NodeId;

    fn bitxor(self, rhs: &NodeId) -> Self::Output {
        NodeId(array::from_fn(|i| self.0[i] ^ rhs.0[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leading_zeros() {
        let mut id = [0u8; 20];
        assert_eq!(NodeId(id).leading_zeros(), 160);

        id[0] = 0x80;
        assert_eq!(NodeId(id).leading_zeros(), 0);

        id[0] = 0x40;
        assert_eq!(NodeId(id).leading_zeros(), 1);

        id[0] = 0x01;
        assert_eq!(NodeId(id).leading_zeros(), 7);

        id[0] = 0x00;
        id[1] = 0x80;
        assert_eq!(NodeId(id).leading_zeros(), 8);
    }

    #[test]
    fn test_distance_exp() {
        let id1 = NodeId([0u8; 20]);
        let mut id2_bytes = [0u8; 20];

        // Same ID -> distance 0 -> lz 160 -> exp 0
        assert_eq!(id1.distance_exp(&NodeId(id2_bytes)), 0);

        // Difference in first bit -> lz 0 -> exp 159
        id2_bytes[0] = 0x80;
        assert_eq!(id1.distance_exp(&NodeId(id2_bytes)), 159);

        // Difference in second bit -> lz 1 -> exp 158
        id2_bytes[0] = 0x40;
        assert_eq!(id1.distance_exp(&NodeId(id2_bytes)), 158);

        // Difference in last bit of first byte -> lz 7 -> exp 152
        id2_bytes[0] = 0x01;
        assert_eq!(id1.distance_exp(&NodeId(id2_bytes)), 152);

        // Difference in first bit of second byte -> lz 8 -> exp 151
        id2_bytes[0] = 0x00;
        id2_bytes[1] = 0x80;
        assert_eq!(id1.distance_exp(&NodeId(id2_bytes)), 151);
    }
}
