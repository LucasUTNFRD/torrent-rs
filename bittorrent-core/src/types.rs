//! Core types for the ``BitTorrent`` daemon API.
//!
//! These types are designed to be serializable for future RPC support
//! and provide a stable interface between the daemon and clients.

use bittorrent_common::types::InfoHash;

/// Torrent identifier - uses ``InfoHash`` for stability across restarts.
///
/// The ``InfoHash`` is the SHA1 hash of the torrent's info dictionary,
/// making it a globally unique and stable identifier.
pub type TorrentId = InfoHash;
