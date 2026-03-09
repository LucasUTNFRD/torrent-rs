use rand::RngCore;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InfoHash([u8; 20]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerID([u8; 20]);

impl Serialize for InfoHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for InfoHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for PeerID {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for PeerID {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

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

    /// Create from Base32 string (RFC 4648, standard alphabet, no padding)
    pub fn from_base32(base32_str: &str) -> Option<Self> {
        let bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, base32_str)?;
        Self::from_slice(&bytes)
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

    /// Identify the client name from the PeerID using BEP 20 conventions
    pub fn identify_client(&self) -> Option<&'static str> {
        let id = &self.0;

        // Azureus-style: -??xxxx- (e.g., -AZ2060-)
        if id[0] == b'-' && id[7] == b'-' {
            let prefix = &id[1..3];
            for (p, name) in AZUREUS_CLIENTS {
                if prefix == *p {
                    return Some(name);
                }
            }
        }

        // Shadow's style: Xxxxx (e.g., S587....)
        let first_byte = id[0];
        for (p, name) in SHADOW_CLIENTS {
            if first_byte == *p {
                return Some(name);
            }
        }

        None
    }
}

const AZUREUS_CLIENTS: &[(&[u8], &str)] = &[
    (b"AG", "Artemis"),
    (b"A~", "Ares"),
    (b"AR", "Arctic"),
    (b"AV", "Avicora"),
    (b"AX", "BitPump"),
    (b"AZ", "Azureus"),
    (b"BB", "BitBuddy"),
    (b"BC", "BitComet"),
    (b"BF", "Bitflu"),
    (b"BG", "BTG (BitTorrent G3)"),
    (b"BR", "BitRocket"),
    (b"BS", "BTSlave"),
    (b"BX", "~BareTorrent"),
    (b"CD", "Enhanced CTorrent"),
    (b"CT", "CTorrent"),
    (b"DE", "DelugeTorrent"),
    (b"DP", "Propagate Data Client"),
    (b"EB", "EBit"),
    (b"ES", "electric sheep"),
    (b"FT", "FoxTorrent"),
    (b"FW", "FrostWire"),
    (b"FX", "Freebox BitTorrent"),
    (b"GS", "GSTorrent"),
    (b"HL", "Halite"),
    (b"HM", "hMule"),
    (b"HN", "Hydranode"),
    (b"KG", "KGet"),
    (b"KT", "KTorrent"),
    (b"LH", "LH-ABC"),
    (b"LP", "LPhant"),
    (b"LT", "libtorrent"),
    (b"lt", "libTorrent"),
    (b"LW", "LimeWire"),
    (b"MO", "MonoTorrent"),
    (b"MP", "MooPolice"),
    (b"MR", "Miro"),
    (b"MT", "MoonlightTorrent"),
    (b"NX", "Net Transport"),
    (b"PD", "Pando"),
    (b"qB", "qBittorrent"),
    (b"QD", "QQDownload"),
    (b"QT", "Qt 4 Torrent example"),
    (b"RT", "Retriever"),
    (b"S~", "Shareaza"),
    (b"SB", "~Swiftbit"),
    (b"SS", "SwarmScope"),
    (b"ST", "SymTorrent"),
    (b"st", "sharktorrent"),
    (b"SZ", "Shareaza"),
    (b"TN", "TorrentDotNet"),
    (b"TR", "Transmission"),
    (b"TS", "Torrentstorm"),
    (b"TT", "TuoTu"),
    (b"UL", "uLeecher!"),
    (b"UT", "µTorrent"),
    (b"UW", "µTorrent Web"),
    (b"VG", "Vagaa"),
    (b"WD", "WebTorrent Desktop"),
    (b"WT", "Bitlet"),
    (b"WY", "FireTorrent"),
    (b"XF", "Xfplay"),
    (b"XL", "Xunlei"),
    (b"XS", "Xirv"),
    (b"XT", "Xanadu"),
    (b"XX", "Xtorrent"),
    (b"ZT", "ZipTorrent"),
];

const SHADOW_CLIENTS: &[(u8, &str)] = &[
    (b'A', "ABC"),
    (b'O', "Osprey Permaseed"),
    (b'Q', "BTQueue"),
    (b'R', "Tribler"),
    (b'S', "Shadow"),
    (b'T', "BitTornado"),
    (b'U', "UPnP NAT BitTorrent"),
];

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identify_client() {
        // qBittorrent
        let mut qb_id = [0u8; 20];
        qb_id[0..8].copy_from_slice(b"-qB4310-");
        let peer_id = PeerID::new(qb_id);
        assert_eq!(peer_id.identify_client(), Some("qBittorrent"));

        // Transmission
        let mut tr_id = [0u8; 20];
        tr_id[0..8].copy_from_slice(b"-TR2940-");
        let peer_id = PeerID::new(tr_id);
        assert_eq!(peer_id.identify_client(), Some("Transmission"));

        // uTorrent
        let mut ut_id = [0u8; 20];
        ut_id[0..8].copy_from_slice(b"-UT3550-");
        let peer_id = PeerID::new(ut_id);
        assert_eq!(peer_id.identify_client(), Some("µTorrent"));

        // Shadow style
        let mut sh_id = [0u8; 20];
        sh_id[0] = b'S';
        let peer_id = PeerID::new(sh_id);
        assert_eq!(peer_id.identify_client(), Some("Shadow"));

        // Unknown
        let mut un_id = [0u8; 20];
        un_id[0..8].copy_from_slice(b"-ZZ9999-");
        let peer_id = PeerID::new(un_id);
        assert_eq!(peer_id.identify_client(), None);
    }
}
