use std::{
    fmt, fs, io,
    net::{IpAddr, Ipv4Addr},
    ops::BitXor,
    path::Path,
};

use bittorrent_common::types::InfoHash;
use crc::{Crc, CRC_32_ISCSI};
use rand::Rng;

const CASTAGNOLI: Crc<u32> = Crc::<u32>::new(&CRC_32_ISCSI);

/// Check if an IPv4 address is exempt from BEP 42 validation.
/// Per BEP 42, local/private/loopback addresses are exempt.
pub fn is_local_ipv4(ip: &Ipv4Addr) -> bool {
    ip.is_private()        // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
        || ip.is_loopback()    // 127.0.0.0/8
        || ip.is_link_local()  // 169.254.0.0/16
        || ip.is_unspecified() // 0.0.0.0
}

// TODO:
// Define a 20-byte NodeId structure.
// Implement the XOR distance calculation (the "closeness" metric).
// Implement the BEP 42 Secure ID generation algorithm.

// ----- NODE TYPE ------
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId([u8; 20]);

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.iter() {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl BitXor for NodeId {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self::Output {
        let mut result = [0u8; 20];

        for (i, byte) in result.iter_mut().enumerate() {
            *byte = self.0[i] ^ rhs.0[i];
        }

        NodeId(result)
    }
}

// InfoHash and NodeId are both 20-byte identifiers.
// This conversion allows using InfoHash for XOR distance calculations in DHT lookups.
impl From<InfoHash> for NodeId {
    fn from(info_hash: InfoHash) -> Self {
        NodeId::from_bytes(*info_hash.as_bytes())
    }
}

impl From<&InfoHash> for NodeId {
    fn from(info_hash: &InfoHash) -> Self {
        NodeId::from_bytes(*info_hash.as_bytes())
    }
}

// TODO: For BEP 00042 we implement this https://www.bittorrent.org/beps/bep_0042.html
// Modern bittorrent clients are using certain type of node_id generators

impl NodeId {
    pub fn from_bytes(bytes: [u8; 20]) -> NodeId {
        NodeId(bytes)
    }

    pub fn generate_random() -> NodeId {
        let mut bytes = [0u8; 20];

        let mut rng = rand::rng();

        rng.fill(&mut bytes);

        Self(bytes)
    }

    pub fn as_bytes(&self) -> [u8; 20] {
        self.0
    }

    pub fn bitlen(&self) -> usize {
        for (i, &byte) in self.0.iter().enumerate() {
            if byte != 0 {
                // We found the first non-zero byte.
                // The number of bits in *subsequent* bytes is (19 - i) * 8
                // Plus the bits in *this* byte: (8 - leading_zeros)
                return (160 - (i * 8)) - (byte.leading_zeros() as usize);
            }
        }
        0
    }

    const V4_MASK: [u8; 4] = [0x03, 0x0f, 0x3f, 0xff];
    const V6_MASK: [u8; 8] = [0x01, 0x03, 0x07, 0x0f, 0x1f, 0x3f, 0x7f, 0xff];
    fn mask_ip(ip: &IpAddr) -> Vec<u8> {
        match ip {
            IpAddr::V4(v4) => {
                let mut o = v4.octets();
                for (i, b) in o.iter_mut().enumerate() {
                    *b &= Self::V4_MASK[i];
                }
                o.to_vec()
            }
            IpAddr::V6(v6) => {
                let mut o = v6.octets();
                for (i, b) in o.iter_mut().enumerate().take(8) {
                    *b &= Self::V6_MASK[i];
                }
                o[..8].to_vec()
            }
        }
    }

    fn apply_r(masked: &mut [u8], r: u8) {
        masked[0] |= r << 5;
    }

    fn crc_prefix(masked: &[u8], r_low3: u8) -> [u8; 3] {
        let mut d = CASTAGNOLI.digest();
        d.update(masked);
        let crc = d.finalize();

        let b0 = (crc >> 24) as u8;
        let b1 = (crc >> 16) as u8;
        let b2 = ((crc >> 8) & 0xf8) as u8 | (r_low3 & 0x7);

        [b0, b1, b2]
    }

    pub fn secure_node_id(&mut self, ip: &IpAddr) {
        let mut rng = rand::rng();

        let rand_byte: u8 = rng.random();
        let r = rand_byte & 0x7;

        let mut masked = Self::mask_ip(ip);
        Self::apply_r(&mut masked, r);

        let prefix = Self::crc_prefix(&masked, rng.random::<u8>());
        self.0[0..3].copy_from_slice(&prefix);

        for i in 3..19 {
            self.0[i] = rng.random();
        }
        self.0[19] = rand_byte;
    }

    #[allow(dead_code)]
    pub(crate) fn from_ipv4_and_r(ip: Ipv4Addr, rand_byte: u8, tail: &[u8; 16]) -> NodeId {
        let mut masked = {
            let mut o = ip.octets();
            for (i, b) in o.iter_mut().enumerate() {
                *b &= Self::V4_MASK[i];
            }
            o.to_vec()
        };

        let r = rand_byte & 0x7;
        masked[0] |= r << 5;

        let r_low3 = tail[0];
        let prefix = Self::crc_prefix(&masked, r_low3);
        // {
        //     let mut d = CASTAGNOLI.digest();
        //     d.update(&masked);
        //     let crc = d.finalize();
        //     let b0 = (crc >> 24) as u8;
        //     let b1 = (crc >> 16) as u8;
        //     let b2 = ((crc >> 8) & 0xf8) as u8 | (tail[0] & 0x7);
        //     [b0, b1, b2]
        // };

        let mut out = [0u8; 20];
        out[0..3].copy_from_slice(&prefix);
        out[3..19].copy_from_slice(&tail[..16]);
        out[19] = rand_byte;

        NodeId(out)
    }

    /// Validate if this node ID is BEP 42 compliant for the given IP address.
    /// Returns true if the node ID's first 21 bits match the expected CRC32C hash.
    /// Local/private IPs are always considered valid (exempt from BEP 42).
    pub fn is_node_id_secure(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => self.is_node_id_secure_v4(&ipv4),
            IpAddr::V6(_) => {
                // IPv6 not supported in this minimal implementation
                // We could implement it, but for now just accept
                true
            }
        }
    }

    /// Check BEP 42 compliance for IPv4.
    pub fn is_node_id_secure_v4(&self, ip: &Ipv4Addr) -> bool {
        // Local IPs are exempt from BEP 42 validation
        if is_local_ipv4(ip) {
            return true;
        }

        // Extract r from byte 19 (the random byte stored during generation)
        let r = self.0[19] & 0x7;

        // Mask the IP address
        let mut masked = ip.octets();
        for (i, b) in masked.iter_mut().enumerate() {
            *b &= Self::V4_MASK[i];
        }

        // Apply r to the first byte: ip[0] |= (r << 5)
        masked[0] |= r << 5;

        // Calculate CRC32C
        let mut d = CASTAGNOLI.digest();
        d.update(&masked);
        let crc = d.finalize();

        // Extract expected prefix (first 21 bits)
        let expected_b0 = (crc >> 24) as u8;
        let expected_b1 = (crc >> 16) as u8;
        let expected_b2_top5 = ((crc >> 8) & 0xf8) as u8;

        // Compare first 21 bits
        self.0[0] == expected_b0
            && self.0[1] == expected_b1
            && (self.0[2] & 0xf8) == expected_b2_top5
    }

    /// Load node ID from a file, or generate a new one if file doesn't exist
    pub fn load_or_generate(path: &Path) -> io::Result<Self> {
        if path.exists() {
            let bytes = fs::read(path)?;
            if bytes.len() == 20 {
                let mut arr = [0u8; 20];
                arr.copy_from_slice(&bytes);
                Ok(NodeId(arr))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid node ID file: expected 20 bytes",
                ))
            }
        } else {
            // Generate new ID and save it
            let id = Self::generate_random();
            id.save(path)?;
            Ok(id)
        }
    }

    /// Save node ID to a file
    pub fn save(&self, path: &Path) -> io::Result<()> {
        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &self.0)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::node_id::{is_local_ipv4, NodeId};
    use std::net::Ipv4Addr;

    #[test]
    fn bep42_vectors() {
        struct V {
            ip: Ipv4Addr,
            r: u8,
            prefix: [u8; 3],
        }
        let tests = [
            V {
                ip: Ipv4Addr::new(124, 31, 75, 21),
                r: 1,
                prefix: [0x5f, 0xbf, 0xbf],
            },
            V {
                ip: Ipv4Addr::new(21, 75, 31, 124),
                r: 86,
                prefix: [0x5a, 0x3c, 0xe9],
            },
            V {
                ip: Ipv4Addr::new(65, 23, 51, 170),
                r: 22,
                prefix: [0xa5, 0xd4, 0x32],
            },
            V {
                ip: Ipv4Addr::new(84, 124, 73, 14),
                r: 65,
                prefix: [0x1b, 0x03, 0x21],
            },
            V {
                ip: Ipv4Addr::new(43, 213, 53, 83),
                r: 90,
                prefix: [0xe5, 0x6f, 0x6c],
            },
        ];

        for t in tests {
            let zero_tail = [0u8; 16];
            let id = NodeId::from_ipv4_and_r(t.ip, t.r, &zero_tail);
            // Check first two bytes exactly
            assert_eq!(id.0[0], t.prefix[0]);
            assert_eq!(id.0[1], t.prefix[1]);
            // For byte 2, only check the top 5 bits (the lower 3 are random)
            assert_eq!(
                id.0[2] & 0xf8,
                t.prefix[2] & 0xf8,
                "Byte 2 top 5 bits mismatch for IP {}",
                t.ip
            );
            assert_eq!(id.0[19], t.r);
        }
    }

    #[test]
    fn is_node_id_secure_validates_correctly() {
        // Test vectors from BEP 42
        struct V {
            ip: Ipv4Addr,
            r: u8,
        }
        let tests = [
            V {
                ip: Ipv4Addr::new(124, 31, 75, 21),
                r: 1,
            },
            V {
                ip: Ipv4Addr::new(21, 75, 31, 124),
                r: 86,
            },
            V {
                ip: Ipv4Addr::new(65, 23, 51, 170),
                r: 22,
            },
            V {
                ip: Ipv4Addr::new(84, 124, 73, 14),
                r: 65,
            },
            V {
                ip: Ipv4Addr::new(43, 213, 53, 83),
                r: 90,
            },
        ];

        for t in tests {
            let zero_tail = [0u8; 16];
            let id = NodeId::from_ipv4_and_r(t.ip, t.r, &zero_tail);

            // A properly generated ID should pass validation
            assert!(
                id.is_node_id_secure_v4(&t.ip),
                "Valid ID should pass for IP {}",
                t.ip
            );

            // Different IP should fail (unless it happens to match, which is unlikely)
            let wrong_ip = Ipv4Addr::new(1, 2, 3, 4);
            assert!(
                !id.is_node_id_secure_v4(&wrong_ip),
                "ID should fail for wrong IP"
            );
        }
    }

    #[test]
    fn local_ips_are_exempt() {
        assert!(is_local_ipv4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_local_ipv4(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_local_ipv4(&Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_local_ipv4(&Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_local_ipv4(&Ipv4Addr::new(169, 254, 1, 1)));

        // Public IPs are not exempt
        assert!(!is_local_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_local_ipv4(&Ipv4Addr::new(1, 1, 1, 1)));
    }

    #[test]
    fn local_ips_always_pass_validation() {
        // Any node ID should pass validation for local IPs
        let random_id = NodeId::generate_random();
        assert!(random_id.is_node_id_secure_v4(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(random_id.is_node_id_secure_v4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(random_id.is_node_id_secure_v4(&Ipv4Addr::new(127, 0, 0, 1)));
    }

    #[test]
    fn test_save_and_load_node_id() {
        use std::env::temp_dir;

        // Create a temporary file path
        let temp_path = temp_dir().join("test_node_id_persistence.tmp");

        // Clean up any existing test file
        let _ = std::fs::remove_file(&temp_path);

        // Generate and save a node ID
        let id1 = NodeId::generate_random();
        id1.save(&temp_path).expect("Failed to save node ID");

        // Load the node ID back
        let id2 = NodeId::load_or_generate(&temp_path).expect("Failed to load node ID");

        // They should be equal
        assert_eq!(
            id1.as_bytes(),
            id2.as_bytes(),
            "Loaded ID should match saved ID"
        );

        // Clean up
        let _ = std::fs::remove_file(&temp_path);
    }

    #[test]
    fn test_load_or_generate_creates_new_if_missing() {
        use std::env::temp_dir;

        // Create a temporary file path that doesn't exist
        let temp_path = temp_dir().join("test_node_id_new.tmp");

        // Clean up any existing test file
        let _ = std::fs::remove_file(&temp_path);

        // Should generate a new ID since file doesn't exist
        let id = NodeId::load_or_generate(&temp_path).expect("Failed to load or generate node ID");

        // Verify the file was created
        assert!(temp_path.exists(), "ID file should be created");

        // Load it again - should get the same ID
        let id2 = NodeId::load_or_generate(&temp_path).expect("Failed to load node ID");
        assert_eq!(
            id.as_bytes(),
            id2.as_bytes(),
            "Loaded ID should match generated ID"
        );

        // Clean up
        let _ = std::fs::remove_file(&temp_path);
    }

    #[test]
    fn test_load_invalid_file_size() {
        use std::env::temp_dir;

        // Create a temporary file with invalid size
        let temp_path = temp_dir().join("test_node_id_invalid.tmp");

        // Write invalid data (not 20 bytes)
        std::fs::write(&temp_path, &[1, 2, 3, 4, 5]).expect("Failed to write test file");

        // Should fail with InvalidData error
        let result = NodeId::load_or_generate(&temp_path);
        assert!(result.is_err(), "Should fail for invalid file size");

        // Clean up
        let _ = std::fs::remove_file(&temp_path);
    }
}
