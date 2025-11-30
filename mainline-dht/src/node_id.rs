use std::{
    fmt,
    net::{IpAddr, Ipv4Addr},
    ops::BitXor,
};

use crc::{CRC_32_ISCSI, Crc};
use rand::Rng;

const CASTAGNOLI: Crc<u32> = Crc::<u32>::new(&CRC_32_ISCSI);

// TODO:
// Define a 20-byte NodeId structure.
// Implement the XOR distance calculation (the "closeness" metric).
// Implement the BEP 42 Secure ID generation algorithm.

// ----- NODE TYPE ------
#[derive(Clone, Copy)]
pub(crate) struct NodeId([u8; 20]);

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.iter() {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
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

// TODO: For BEP 00042 we implement this https://www.bittorrent.org/beps/bep_0042.html
// Modern bittorrent clients are using certain type of node_id generators

impl NodeId {
    pub(crate) fn generate_random() -> NodeId {
        let mut bytes = [0u8; 20];

        let mut rng = rand::rng();

        rng.fill(&mut bytes);

        Self(bytes)
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

    pub(crate) fn secure_node_id(&mut self, ip: &IpAddr) {
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

    pub fn is_node_id_secure(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => {
                if ipv4.is_private() || ipv4.is_link_local() || ipv4.is_loopback() {
                    return true;
                }
                // crc32c((ip & 0x030f3fff) | (r << 29))
            }
            IpAddr::V6(ipv6) => {
                if ipv6.is_loopback() || ipv6.is_unicast_link_local() {
                    return true;
                }
                // crc32c((ip & 0x0103070f1f3f7fff) | (r << 61))
            }
        }

        todo!()
    }
}

#[cfg(test)]
mod test {
    use crate::node_id::NodeId;

    #[test]
    fn bep42_vectors() {
        use std::net::Ipv4Addr;

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
}
