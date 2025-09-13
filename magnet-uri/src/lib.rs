use std::{fmt, net::SocketAddr, str::FromStr};

use bittorrent_common::types::InfoHash;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerAddr {
    Socket(SocketAddr),
    HostPort { host: String, port: u16 }, // keep hostname for clients that want name-resolution later
}

impl FromStr for PeerAddr {
    type Err = MagnetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(socket) = s.parse::<SocketAddr>() {
            return Ok(PeerAddr::Socket(socket));
        }

        if let Some((host, port_str)) = s.rsplit_once(':')
            && let Ok(port) = port_str.parse::<u16>()
        {
            return Ok(PeerAddr::HostPort {
                host: host.to_string(),
                port,
            });
        }

        Err(MagnetError::InvalidPeer(s.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Magnet {
    pub hash_type: HashType,
    pub display_name: Option<String>,
    pub trackers: Vec<Url>,   // tr entries
    pub peers: Vec<PeerAddr>, // x.pe entries
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashType {
    InfoHash(InfoHash),
    MultiHash,
}

impl fmt::Display for PeerAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerAddr::Socket(addr) => write!(f, "{}", addr),
            PeerAddr::HostPort { host, port } => write!(f, "{}:{}", host, port),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum MagnetError {
    #[error("missing xt parameter")]
    MissingXt,
    #[error("invalid infohash: {0}")]
    InvalidInfoHash(String),
    #[error("invalid tracker url: {0}")]
    InvalidTracker(String),
    #[error("invalid peer address: {0}")]
    InvalidPeer(String),
    #[error("url parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("invalid scheme: {0}")]
    InvalidScheme(String),
    #[error("Invalid Manget URI")]
    InvalidMagnet,
}

const HEX_ENCODED_LEN: usize = 40;
const BASE32_ENCODED_LEN: usize = 32;

impl FromStr for Magnet {
    type Err = MagnetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let url = Url::parse(s)?;

        if url.scheme() != "magnet" {
            return Err(MagnetError::InvalidScheme(url.scheme().to_string()));
        }

        let mut hash_type = None;
        let mut display_name = None;
        let mut trackers = Vec::new();
        let mut peers = Vec::new();

        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "xt" => {
                    // Only support v1 ("urn:btih:") for now
                    if let Some(rest) = value.strip_prefix("urn:btih:") {
                        hash_type = match rest.len() {
                            HEX_ENCODED_LEN => Some(HashType::InfoHash(
                                InfoHash::from_hex(rest)
                                    .map_err(|_| MagnetError::InvalidInfoHash(rest.to_string()))?,
                            )),
                            BASE32_ENCODED_LEN => Some(HashType::InfoHash(
                                InfoHash::from_base32(rest)
                                    .ok_or(MagnetError::InvalidInfoHash(rest.to_string()))?,
                            )),
                            _ => return Err(MagnetError::InvalidInfoHash(rest.to_string())),
                        };
                    } else {
                        // Ignore other URNs like v2 (btmh)
                        continue;
                    }
                }
                "dn" => {
                    display_name = Some(value.into_owned());
                }
                "tr" => {
                    let tracker = Url::parse(&value)
                        .map_err(|_| MagnetError::InvalidTracker(value.to_string()))?;
                    trackers.push(tracker);
                }
                "x.pe" => {
                    let peer = value.parse::<PeerAddr>()?;
                    peers.push(peer);
                }
                _ => {} // Ignore unknown parameters
            }
        }

        if hash_type.is_none() {
            return Err(MagnetError::MissingXt);
        }

        let hash_type = hash_type.unwrap();

        Ok(Magnet {
            hash_type,
            display_name,
            trackers,
            peers,
        })
    }
}

#[cfg(test)]
mod test {
    use std::net::{Ipv4Addr, SocketAddr};

    use bittorrent_common::types::InfoHash;
    use url::Url;

    use crate::{HashType, Magnet, PeerAddr};

    #[test]
    fn parse_magnet_uri() {
        const MAGNET_STR: &str = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Sintel&tr=udp%3A%2F%2Fexplodie.org%3A6969&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969&tr=udp%3A%2F%2Ftracker.empire-js.us%3A1337&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=wss%3A%2F%2Ftracker.btorrent.xyz&tr=wss%3A%2F%2Ftracker.fastcast.nz&tr=wss%3A%2F%2Ftracker.openwebtorrent.com&ws=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2F&xs=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2Fsintel.torrent";
        let magnet_link: Magnet = MAGNET_STR.parse().unwrap();

        assert_eq!(magnet_link.display_name, Some("Sintel".to_string()));
        assert!(matches!(magnet_link.hash_type, HashType::InfoHash(_)));

        assert_eq!(
            magnet_link.hash_type,
            HashType::InfoHash(
                InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap()
            )
        );

        let expected_trackers = [
            "udp://explodie.org:6969",
            "udp://tracker.coppersurfer.tk:6969",
            "udp://tracker.empire-js.us:1337",
            "udp://tracker.leechers-paradise.org:6969",
            "udp://tracker.opentrackr.org:1337",
            "wss://tracker.btorrent.xyz/",
            "wss://tracker.fastcast.nz/",
            "wss://tracker.openwebtorrent.com/",
        ];

        for (expected_tracker, tracker) in expected_trackers
            .iter()
            .map(|t| Url::parse(t).unwrap())
            .zip(magnet_link.trackers)
        {
            assert_eq!(expected_tracker, tracker);
        }
    }

    #[test]
    fn parse_magnet_peers() {
        const MAGNET_STR: &str = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&x.pe=127.0.0.1:6881&x.pe=myhomepc.local:1337";

        let magnet = MAGNET_STR.parse::<Magnet>().unwrap();

        let expected_peers = vec![
            PeerAddr::Socket(SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 6881)),
            PeerAddr::HostPort {
                host: "myhomepc.local".to_string(),
                port: 1337,
            },
        ];

        assert_eq!(magnet.peers, expected_peers);
    }

    #[test]
    fn parse_magnet_uri_base32_infohash() {
        // Create expected infohash from the same hex used in other tests
        let expected_infohash =
            InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();

        // Convert the same hex hash to bytes, then encode as base32 to get the correct base32 representation
        let bytes = hex::decode("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
        let base32_hash = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &bytes);

        // Verify the base32 string is the expected length (32 characters for 20-byte hash)
        assert_eq!(base32_hash.len(), 32);

        // Verify that InfoHash::from_base32 can parse this base32 string correctly
        let infohash_from_base32 = InfoHash::from_base32(&base32_hash).unwrap();
        assert_eq!(expected_infohash, infohash_from_base32);

        // Create magnet URI with base32 infohash
        let magnet_str = format!("magnet:?xt=urn:btih:{}&dn=TestFile", base32_hash);
        let magnet = magnet_str.parse::<Magnet>().unwrap();

        // Verify the parsed magnet has the correct infohash
        assert_eq!(magnet.display_name, Some("TestFile".to_string()));
        assert_eq!(magnet.hash_type, HashType::InfoHash(expected_infohash));
    }
}
