//! Core types for the ``BitTorrent`` daemon API.
//!
//! These types are designed to be serializable for future RPC support
//! and provide a stable interface between the daemon and clients.

use std::path::PathBuf;

use bittorrent_common::types::InfoHash;

/// Torrent identifier - uses ``InfoHash`` for stability across restarts.
///
/// The ``InfoHash`` is the SHA1 hash of the torrent's info dictionary,
/// making it a globally unique and stable identifier.
pub type TorrentId = InfoHash;

/// Current state of a torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
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
    /// Total size in bytes
    pub size_bytes: u64,
    /// Bytes downloaded so far
    pub downloaded_bytes: u64,
}

/// Aggregate statistics for the entire session.
#[derive(Debug, Clone, Default)]
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

/// Configuration for creating a new Session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Port to listen on for incoming peer connections
    pub port: u16,
    /// Directory where downloaded files are saved
    pub save_path: PathBuf,
    /// Whether to enable DHT for peer discovery
    pub enable_dht: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            port: 6881,
            save_path: PathBuf::from("."),
            enable_dht: true,
        }
    }
}
