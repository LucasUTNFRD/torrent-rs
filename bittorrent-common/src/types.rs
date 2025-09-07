use rand::{Rng, RngCore, distr::Alphanumeric};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InfoHash([u8; 20]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerID([u8; 20]);

impl InfoHash {
    /// Create a new InfoHash from a 20-byte array
    pub fn new(hash: [u8; 20]) -> Self {
        Self(hash)
    }

    /// Create InfoHash from a slice (returns None if not exactly 20 bytes)
    pub fn from_slice(slice: &[u8]) -> Option<Self> {
        if slice.len() == 20 {
            let mut array = [0u8; 20];
            array.copy_from_slice(slice);
            Some(Self(array))
        } else {
            None
        }
    }

    /// Get the underlying byte array
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Convert to a byte slice
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Create from hex string
    pub fn from_hex(hex_str: &str) -> Result<Self, hex::FromHexError> {
        let bytes = hex::decode(hex_str)?;
        Self::from_slice(&bytes).ok_or(hex::FromHexError::InvalidStringLength)
    }
}

impl PeerID {
    /// Create a new PeerID from a 20-byte array
    pub fn new(id: [u8; 20]) -> Self {
        Self(id)
    }

    /// Generate a BEP 20 peer ID
    /// (BEP 20)-[https://www.bittorrent.org/beps/bep_0020.html]
    pub fn generate() -> Self {
        const BEP20: &[u8] = b"-RS0000-"; // 8-byte prefix
        let mut id = [0u8; 20];

        // Copy prefix into the start of id
        let o = BEP20.len();
        id[..o].copy_from_slice(BEP20);

        // Fill remaining bytes with random bytes
        rand::rng().fill_bytes(&mut id[o..]);

        Self(id)
    }

    /// Create PeerID from a slice (returns None if not exactly 20 bytes)
    pub fn from_slice(slice: &[u8]) -> Option<Self> {
        if slice.len() == 20 {
            let mut array = [0u8; 20];
            array.copy_from_slice(slice);
            Some(Self(array))
        } else {
            None
        }
    }

    /// Get the underlying byte array
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Convert to a byte slice
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Convert to hex string  
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Create from hex string
    pub fn from_hex(hex_str: &str) -> Result<Self, hex::FromHexError> {
        let bytes = hex::decode(hex_str)?;
        Self::from_slice(&bytes).ok_or(hex::FromHexError::InvalidStringLength)
    }
}

// Implement From traits for convenient conversion
impl From<[u8; 20]> for InfoHash {
    fn from(hash: [u8; 20]) -> Self {
        Self::new(hash)
    }
}

impl From<[u8; 20]> for PeerID {
    fn from(id: [u8; 20]) -> Self {
        Self::new(id)
    }
}

// Implement AsRef for easy access to underlying data
impl AsRef<[u8]> for InfoHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for PeerID {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

// Optional: Display implementations for pretty printing
impl std::fmt::Display for InfoHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl std::fmt::Display for PeerID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}
