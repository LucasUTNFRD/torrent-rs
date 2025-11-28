use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    ops::BitXor,
};

use bittorrent_common::types::InfoHash;
use crc32c::{Crc32cHasher, crc32c};
use rand::Rng;

// TODO:
// Define a 20-byte NodeId structure.
// Implement the XOR distance calculation (the "closeness" metric).
// Implement the BEP 42 Secure ID generation algorithm.

// ----- NODE TYPE ------
#[derive(Clone, Copy)]
struct NodeId([u8; 20]);

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

impl NodeId {
    pub fn generate_random() -> NodeId {
        let mut bytes = [0u8; 20];

        let mut rng = rand::rng();

        rng.fill(&mut bytes);

        Self(bytes)
    }

    fn secure_node_id(&mut self, ip: &IpAddr) {
        let mut ip_bytes: Vec<u8> = match ip {
            IpAddr::V4(v4) => {
                const V4_MASK: [u8; 4] = [0x03, 0x0f, 0x3f, 0xff];
                let mut octets = v4.octets();
                for (i, octet) in octets.iter_mut().enumerate() {
                    *octet &= V4_MASK[i];
                }
                octets.to_vec()
            }
            IpAddr::V6(v6) => {
                const V6_MASK: [u8; 8] = [0x01, 0x03, 0x07, 0x0f, 0x1f, 0x3f, 0x7f, 0xff];
                let mut octets = v6.octets();
                for (i, octet) in octets.iter_mut().enumerate().take(8) {
                    *octet &= V6_MASK[i];
                }
                octets[..8].to_vec()
            }
        };

        let mut rng = rand::rng();
        let rand: u8 = rng.random();
        let r: u8 = rand & 0x7;
        ip_bytes[0] |= r << 5;

        let crc = crc32c(&ip_bytes);

        // let mut node_id = [0u8; 20];
        self.0[0] = (crc >> 24) as u8;
        self.0[1] = (crc >> 16) as u8;
        self.0[2] = ((crc >> 8) & 0xf8) as u8 | (rng.random::<u8>() & 0x7);

        for i in 3..19 {
            self.0[i] = rng.random();
        }
        self.0[19] = rand;
    }
}

// TODO: For BEP 00042 we implement this https://www.bittorrent.org/beps/bep_0042.html
// Modern bittorrent clients are using certain type of node_id generators

// ---- SERVER ----

#[derive(Debug, Clone, Copy)]
pub struct Server {
    node_id: NodeId,
    listen_addr: SocketAddr,
}

pub struct Node {}

impl Server {
    pub fn new(listen_addr: SocketAddr) -> Self {
        let mut node_id = NodeId::generate_random();
        node_id.secure_node_id(&listen_addr.ip());

        Self {
            node_id,
            listen_addr,
        }
    }
}

#[cfg(test)]
mod test {

    #[test]
    fn test_secure_node_id_generation() {
        todo!();
    }
}
