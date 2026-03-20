//! Data Transfer Objects for torrent detail views.
//!
//! These types are designed for the TUI and future RPC/API consumers,
//! providing a stable interface to query torrent state.

use std::path::PathBuf;

use bittorrent_common::{metainfo::TorrentInfo, types::InfoHash};

/// Static torrent metadata that doesn't change after the torrent is added.
///
/// Set once when metadata is fetched, never changes after that.
#[derive(Debug, Clone)]
pub struct TorrentMeta {
    /// SHA1 hash of the info dictionary
    pub info_hash: InfoHash,
    /// Display name from torrent metadata
    pub name: String,
    /// Total size in bytes
    pub total_bytes: u64,
    /// Piece length in bytes
    pub piece_length: u32,
    /// Number of pieces
    pub num_pieces: u32,
    /// Number of files (1 for single-file torrents)
    pub num_files: usize,
    /// Whether this is a private torrent
    pub is_private: bool,
    /// Optional comment from .torrent file
    pub comment: Option<String>,
    /// Optional creator from .torrent file
    pub created_by: Option<String>,
}

impl TorrentMeta {
    /// Create from a parsed TorrentInfo
    pub fn from_torrent_info(info_hash: InfoHash, torrent_info: &TorrentInfo) -> Self {
        Self {
            info_hash,
            name: torrent_info.info.mode.name().to_string(),
            total_bytes: torrent_info.info.total_size() as u64,
            piece_length: torrent_info.info.piece_length as u32,
            num_pieces: torrent_info.info.pieces.len() as u32,
            num_files: torrent_info.info.mode.files().len(),
            is_private: torrent_info.is_private(),
            comment: torrent_info.comment.clone(),
            created_by: torrent_info.created_by.clone(),
        }
    }

    /// Create from Info hash and Info struct (for magnet links after metadata fetched)
    pub fn from_info(info_hash: InfoHash, info: &bittorrent_common::metainfo::Info) -> Self {
        Self {
            info_hash,
            name: info.mode.name().to_string(),
            total_bytes: info.total_size() as u64,
            piece_length: info.piece_length as u32,
            num_pieces: info.pieces.len() as u32,
            num_files: info.mode.files().len(),
            is_private: info.private == Some(1),
            comment: None,
            created_by: None,
        }
    }
}

/// File information with download progress.
///
/// Unlike `bittorrent_common::metainfo::FileInfo`, this includes runtime
/// progress information about how much of each file has been downloaded.
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// File index in the torrent
    pub index: usize,
    /// Path relative to the torrent root
    pub path: PathBuf,
    /// Total file size in bytes
    pub size_bytes: u64,
    /// Bytes downloaded (verified pieces covering this file)
    pub downloaded_bytes: u64,
}

/// Extension flags indicating peer protocol support.
#[derive(Debug, Clone, Default)]
pub struct ExtensionFlags {
    /// DHT support
    pub dht: bool,
    /// Extension protocol support (BEP 10)
    pub extension_protocol: bool,
    /// Metadata exchange support (BEP 9)
    pub metadata: bool,
    /// Peer exchange support (BEP 11)
    pub pex: bool,
}

/// Detailed peer connection state for the UI.
#[derive(Debug, Clone)]
pub struct PeerInfoSnapshot {
    /// Whether the peer is choking us (we can't download)
    pub remote_choking: bool,
    /// Whether the peer is interested in us (wants data)
    pub remote_interested: bool,
    /// Whether we are choking the peer (they can't download)
    pub am_choking: bool,
    /// Whether we are interested in the peer (want data)
    pub am_interested: bool,
    /// Connection direction
    pub source: Direction,
    /// Download rate (bytes/sec)
    pub download_rate: f32,
    /// Upload rate (bytes/sec)
    pub upload_rate: f32,
    /// Peer's progress (0.0 - 1.0)
    pub peer_progress: f32,
    /// Peer client identification string
    pub client_name: String,
    /// Extension flags
    pub extension_flags: ExtensionFlags,
}

/// Snapshot of a connected peer's state for the UI.
#[derive(Debug, Clone)]
pub struct PeerSnapshot {
    /// Peer address and port
    pub addr: std::net::SocketAddr,
    /// Detailed peer info
    pub info: PeerInfoSnapshot,
}

/// Status of a tracker for a torrent.
#[derive(Debug, Clone)]
pub struct TrackerStatus {
    /// Tracker announce URL
    pub url: String,
    /// Tracker tier (lower = higher priority, None if unknown)
    pub tier: Option<usize>,
    /// Number of seeders reported by tracker (if known)
    pub seeder_count: Option<u32>,
    /// Number of leechers reported by tracker (if known)
    pub leecher_count: Option<u32>,
    /// Number of peers received from last announce
    pub peers_received: u32,
    /// Current tracker state
    pub status: TrackerState,
    /// Last error message (if any)
    pub last_error: Option<String>,
}

/// Connection direction for a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// We connected to this peer
    Outbound,
    /// This peer connected to us
    Inbound,
}

/// Tracker connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerState {
    /// Not yet contacted
    Idle,
    /// Currently announcing
    Announcing,
    /// Successfully announced
    Ok,
    /// Last announce failed
    Error,
}

/// Complete torrent detail for the TUI detail view.
///
/// Contains all information needed to render the detail tabs:
/// overview, files, peers, and trackers.
#[derive(Debug, Clone)]
pub struct TorrentDetail {
    /// Static metadata
    pub meta: TorrentMeta,
    /// List of files with progress
    pub files: Vec<FileInfo>,
    /// Connected peers
    pub peers: Vec<PeerSnapshot>,
    /// Tracker statuses  
    pub trackers: Vec<TrackerStatus>,
}
