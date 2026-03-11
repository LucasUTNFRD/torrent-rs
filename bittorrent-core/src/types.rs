//! Core types for the ``BitTorrent`` daemon API.
//!
//! These types are designed to be serializable for future RPC support
//! and provide a stable interface between the daemon and clients.

use bittorrent_common::types::InfoHash;
use serde::{Deserialize, Serialize};

/// Torrent identifier - uses ``InfoHash`` for stability across restarts.
///
/// The ``InfoHash`` is the SHA1 hash of the torrent's info dictionary,
/// making it a globally unique and stable identifier.
pub type TorrentId = InfoHash;

/// Current state of a torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TorrentState {
    /// Magnet link - fetching metadata from peers
    FetchingMetadata,
    /// Verifying existing pieces on disk
    Checking,
    /// Actively downloading pieces
    Downloading,
    /// Download complete, sharing with peers
    Seeding,
    /// Torrent is paused
    Paused,
    /// An error occurred
    Error,
}

impl std::fmt::Display for TorrentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchingMetadata => write!(f, "Fetching Metadata"),
            Self::Checking => write!(f, "Checking"),
            Self::Downloading => write!(f, "Downloading"),
            Self::Seeding => write!(f, "Seeding"),
            Self::Paused => write!(f, "Paused"),
            Self::Error => write!(f, "Error"),
        }
    }
}

/// Summary information about a torrent.
///
/// This provides a snapshot of a torrent's current state and progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentSummary {
    /// Unique identifier ``InfoHash``
    pub id: TorrentId,
    /// Display name of the torrent
    pub name: String,
    /// Current state
    pub state: TorrentState,
    /// Download progress as a fraction (0.0 - 1.0)
    pub progress: f64,
    /// Current download rate in bytes per second
    pub download_rate: u64,
    /// Current upload rate in bytes per second
    pub upload_rate: u64,
    /// Number of currently connected peers
    pub peers_connected: usize,
    /// Total number of peers discovered (from trackers/DHT)
    pub peers_discovered: usize,
    /// Total size in bytes
    pub size_bytes: u64,
    /// Bytes downloaded so far
    pub downloaded_bytes: u64,
}

/// Detailed information about a peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: String,        // Hex string of PeerID
    pub client_id: String, // Client name/version if known
    pub ip: String,
    pub rate_up: u64,   // bits/sec
    pub rate_down: u64, // bits/sec
}

/// Information about a tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerInfo {
    pub url: String,
    pub error: Option<String>,
    pub last_report: Option<chrono::DateTime<chrono::Utc>>,
}

/// Information about a file within a torrent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub progress: f64,
}

/// Detailed information about a torrent, including peers, trackers, and files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentDetails {
    #[serde(flatten)]
    pub summary: TorrentSummary,
    pub peers: Vec<PeerInfo>,
    pub trackers: Vec<TrackerInfo>,
    pub files: Vec<FileInfo>,
}

/// Aggregate statistics for the entire session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStats {
    /// Number of torrents currently downloading
    pub torrents_downloading: usize,
    /// Number of torrents currently seeding
    pub torrents_seeding: usize,
    /// Total download rate across all torrents (bytes/sec)
    pub total_download_rate: u64,
    /// Total upload rate across all torrents (bytes/sec)
    pub total_upload_rate: u64,
    /// Number of nodes in the DHT routing table (None if DHT disabled)
    pub dht_nodes: Option<usize>,
}
