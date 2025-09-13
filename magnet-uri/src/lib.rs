//! # magnet-uri
//!
//! A library for parsing and working with BitTorrent [Magnet URI](http://bittorrent.org/beps/bep_0009.html) links.
//!
//! Magnet URIs are a way to identify and retrieve resources in peer-to-peer networks without
//! the need for a .torrent file. They contain metadata like the infohash, display name, trackers,
//! and other optional fields.
//!
//! ## Format
//!
//! A magnet URI typically has the following format:
//!
//! ```text
//! magnet:?xt=urn:btih:<infohash>&dn=<name>&tr=<tracker>&tr=<tracker>...
//! ```
//!
//! Where:
//! - `xt` (exact topic): Contains the BitTorrent infohash
//! - `dn` (display name): Optional name for the resource
//! - `tr` (tracker): Optional tracker URLs
//! - `x.pe` (peer): Optional peer addresses
//!
//! ## Examples
//!
//! ```
//! use std::str::FromStr;
//! use magnet_uri::Magnet;
//!
//! // Parse a magnet URI
//! let uri = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Example";
//! let magnet = Magnet::from_str(uri).expect("Failed to parse magnet URI");
//!
//! println!("Display name: {}", magnet.display_name.unwrap_or_default());
//! println!("Number of trackers: {}", magnet.trackers.len());
//! ```

use std::{fmt, net::SocketAddr, str::FromStr};

use bittorrent_common::types::InfoHash;
use url::Url;

/// Represents a peer address which can be either a socket address (IP:port)
/// or a hostname and port combination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerAddr {
    /// A socket address (IP:port)
    Socket(SocketAddr),

    /// A hostname and port combination
    ///
    /// This variant is used when the peer address contains a hostname that needs
    /// to be resolved later, rather than an IP address.
    HostPort {
        /// The hostname part of the peer address
        host: String,
        /// The port number
        port: u16,
    },
}

impl FromStr for PeerAddr {
    type Err = MagnetError;

    /// Parses a string into a `PeerAddr`.
    ///
    /// The string should be in the format "hostname:port" or a valid socket address.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    /// use magnet_uri::PeerAddr;
    ///
    /// let socket = PeerAddr::from_str("192.168.1.1:6881").unwrap();
    /// let host_port = PeerAddr::from_str("tracker.example.com:6969").unwrap();
    /// ```
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

/// Represents a parsed BitTorrent magnet URI.
///
/// A magnet URI contains metadata about a torrent, including its infohash,
/// optional display name, tracker URLs, and peer addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Magnet {
    /// The hash type contained in the magnet URI
    pub hash_type: HashType,

    /// Optional display name of the torrent
    pub display_name: Option<String>,

    /// List of tracker URLs (from tr= parameters)
    pub trackers: Vec<Url>,

    /// List of peer addresses (from x.pe= parameters)
    pub peers: Vec<PeerAddr>,
}

/// Represents the type of hash contained in a magnet URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashType {
    /// A BitTorrent v1 infohash (SHA-1, 20 bytes)
    InfoHash(InfoHash),

    /// A BitTorrent v2 multihash (not fully implemented yet)
    ///
    /// This is a placeholder for BitTorrent v2 support, which uses
    /// different hash algorithms.
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

impl fmt::Display for Magnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_uri())
    }
}

/// Errors that can occur when parsing magnet URIs.
#[derive(thiserror::Error, Debug)]
pub enum MagnetError {
    /// The required xt parameter is missing
    #[error("missing xt parameter")]
    MissingXt,

    /// The infohash is invalid
    #[error("invalid infohash: {0}")]
    InvalidInfoHash(String),

    /// A tracker URL is invalid
    #[error("invalid tracker url: {0}")]
    InvalidTracker(String),

    /// A peer address is invalid
    #[error("invalid peer address: {0}")]
    InvalidPeer(String),

    /// Error parsing the URL
    #[error("url parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    /// The URI scheme is not "magnet"
    #[error("invalid scheme: {0}")]
    InvalidScheme(String),

    /// General invalid magnet URI error
    #[error("Invalid Magnet URI")]
    InvalidMagnet,
}

// Constants for hash encoding lengths
const HEX_ENCODED_LEN: usize = 40;
const BASE32_ENCODED_LEN: usize = 32;

impl FromStr for Magnet {
    type Err = MagnetError;

    /// Parses a string into a `Magnet`.
    ///
    /// The string should be a valid magnet URI starting with "magnet:?".
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    /// use magnet_uri::Magnet;
    ///
    /// let uri = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Example";
    /// let magnet = Magnet::from_str(uri).expect("Failed to parse magnet URI");
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a `MagnetError` if:
    /// - The URI is not a valid URL
    /// - The scheme is not "magnet"
    /// - The required "xt" parameter is missing
    /// - The infohash format is invalid
    /// - Any tracker URLs are invalid
    /// - Any peer addresses are invalid
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

impl Magnet {
    /// Creates a new `Magnet` with the specified infohash.
    ///
    /// # Examples
    ///
    /// ```
    /// use bittorrent_common::types::InfoHash;
    /// use magnet_uri::{Magnet, HashType};
    ///
    /// let infohash = InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
    /// let magnet = Magnet::new(infohash);
    /// ```
    pub fn new(infohash: InfoHash) -> Self {
        Self {
            hash_type: HashType::InfoHash(infohash),
            display_name: None,
            trackers: Vec::new(),
            peers: Vec::new(),
        }
    }

    /// Sets the display name for this magnet URI.
    ///
    /// # Examples
    ///
    /// ```
    /// use bittorrent_common::types::InfoHash;
    /// use magnet_uri::{Magnet, HashType};
    ///
    /// let infohash = InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
    /// let magnet = Magnet::new(infohash).with_display_name("My Torrent");
    /// ```
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Adds a tracker URL to this magnet URI.
    ///
    /// # Examples
    ///
    /// ```
    /// use bittorrent_common::types::InfoHash;
    /// use magnet_uri::{Magnet, HashType};
    /// use url::Url;
    ///
    /// let infohash = InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
    /// let tracker = Url::parse("udp://tracker.example.com:6969").unwrap();
    /// let magnet = Magnet::new(infohash).add_tracker(tracker);
    /// ```
    pub fn add_tracker(mut self, tracker: Url) -> Self {
        self.trackers.push(tracker);
        self
    }

    /// Adds a peer address to this magnet URI.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::net::{SocketAddr, IpAddr, Ipv4Addr};
    /// use bittorrent_common::types::InfoHash;
    /// use magnet_uri::{Magnet, HashType, PeerAddr};
    ///
    /// let infohash = InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
    /// let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6881);
    /// let magnet = Magnet::new(infohash).add_peer(PeerAddr::Socket(socket));
    /// ```
    pub fn add_peer(mut self, peer: PeerAddr) -> Self {
        self.peers.push(peer);
        self
    }

    /// Gets the infohash, if this magnet URI contains a BitTorrent v1 infohash.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    /// use magnet_uri::Magnet;
    ///
    /// let uri = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10";
    /// let magnet = Magnet::from_str(uri).unwrap();
    /// let infohash = magnet.infohash().unwrap();
    /// ```
    pub fn info_hash(&self) -> Option<InfoHash> {
        match self.hash_type {
            HashType::InfoHash(hash) => Some(hash),
            _ => None,
        }
    }

    /// Converts the Magnet struct back to a URI string.
    ///
    /// This creates a valid magnet URI that can be shared with others.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    /// use magnet_uri::Magnet;
    ///
    /// let uri = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Example";
    /// let magnet = Magnet::from_str(uri).unwrap();
    /// let uri_string = magnet.to_uri();
    /// ```
    pub fn to_uri(&self) -> String {
        let mut uri = String::from("magnet:?");

        // Add the infohash
        match self.hash_type {
            HashType::InfoHash(hash) => {
                uri.push_str("xt=urn:btih:");
                uri.push_str(&hash.to_hex());
            }
            HashType::MultiHash => {
                // Not implemented for now
            }
        }

        // Add display name if present
        if let Some(name) = &self.display_name {
            uri.push_str("&dn=");
            // URL encode the name
            uri.push_str(
                &url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>(),
            );
        }

        // Add trackers
        for tracker in &self.trackers {
            uri.push_str("&tr=");
            // URL encode the tracker URL
            uri.push_str(
                &url::form_urlencoded::byte_serialize(tracker.as_str().as_bytes())
                    .collect::<String>(),
            );
        }

        // Add peers
        for peer in &self.peers {
            uri.push_str("&x.pe=");
            // URL encode the peer address
            let peer_str = peer.to_string();
            uri.push_str(
                &url::form_urlencoded::byte_serialize(peer_str.as_bytes()).collect::<String>(),
            );
        }

        uri
    }
}

#[cfg(test)]
mod test {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        str::FromStr,
    };

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

    #[test]
    fn test_builder_methods() {
        let infohash = InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();

        let tracker1 = Url::parse("udp://tracker1.example.com:6969").unwrap();
        let tracker2 = Url::parse("udp://tracker2.example.com:6969").unwrap();

        let peer1 = PeerAddr::Socket(SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 6881));
        let peer2 = PeerAddr::HostPort {
            host: "peer.example.com".to_string(),
            port: 1337,
        };

        let magnet = Magnet::new(infohash)
            .with_display_name("Example Torrent")
            .add_tracker(tracker1)
            .add_tracker(tracker2)
            .add_peer(peer1)
            .add_peer(peer2);

        assert_eq!(magnet.display_name, Some("Example Torrent".to_string()));
        assert_eq!(magnet.trackers.len(), 2);
        assert_eq!(magnet.peers.len(), 2);
        assert_eq!(magnet.info_hash(), Some(infohash));

        // Test Display implementation
        let display_output = format!("{}", magnet);
        assert_eq!(display_output, magnet.to_uri());
    }

    #[test]
    fn test_to_uri() {
        let infohash = InfoHash::from_hex("08ada5a7a6183aae1e09d831df6748d566095a10").unwrap();
        let magnet = Magnet::new(infohash).with_display_name("Test File");

        let uri = magnet.to_uri();

        // Parse the URI back to verify it roundtrips correctly
        let parsed_magnet = Magnet::from_str(&uri).unwrap();

        assert_eq!(parsed_magnet.display_name, Some("Test File".to_string()));
        assert_eq!(parsed_magnet.hash_type, HashType::InfoHash(infohash));

        // Test with trackers and peers
        let tracker = Url::parse("udp://tracker.example.com:6969").unwrap();
        let peer = PeerAddr::Socket(SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 6881));

        let magnet_with_extras = Magnet::new(infohash)
            .with_display_name("Test File")
            .add_tracker(tracker.clone())
            .add_peer(peer.clone());

        let uri_with_extras = magnet_with_extras.to_uri();
        let parsed_with_extras = Magnet::from_str(&uri_with_extras).unwrap();

        assert_eq!(
            parsed_with_extras.display_name,
            Some("Test File".to_string())
        );
        assert_eq!(parsed_with_extras.trackers.len(), 1);
        assert_eq!(parsed_with_extras.trackers[0], tracker);
        assert_eq!(parsed_with_extras.peers.len(), 1);
    }
}
