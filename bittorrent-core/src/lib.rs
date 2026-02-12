//! # bittorrent-core
//!
//! Core ``BitTorrent`` client implementation with DHT and tracker-based peer discovery.
//!
//! ## Architecture
//!
//! ```text
//! Session
//!    │
//!    ├── TrackerHandler ──────────────────────┐
//!    ├── DhtHandler (mainline DHT) ───────────┼── Arc<_> shared
//!    └── Storage ─────────────────────────────┘
//!                 │
//!       ┌─────────┼─────────┐
//!       ▼         ▼         ▼
//!    Torrent   Torrent   Torrent
//!       │
//!       └── Peer connections + piece management
//! ```
//!
//! ## Modules
//!
//! - **session**: Entry point. Manages torrents, accepts incoming connections, bootstraps DHT
//! - **torrent**: Per-torrent state machine handling peers, pieces, and announcements
//! - **peer**: Peer connection handling and the ``BitTorrent`` wire protocol
//! - **piece_picker**: Piece selection strategy and block request management
//! - **storage**: Disk I/O for reading/writing pieces
//! - **bitfield**: Compact representation of available pieces
//! - **metadata**: BEP 9 metadata exchange for magnet links
//! - **choker**: manages which peers are choked/unchoked (currently round-robin)
//!
//! ## Peer Discovery
//!
//! Peers are discovered via two parallel mechanisms:
//! 1. **Trackers** - HTTP/UDP announces to tracker URLs from the torrent file
//! 2. **DHT** - Mainline DHT for trackerless/magnet link support
//!
//! Both sources feed peers into the same channel for connection attempts.

mod bitfield;
mod choker;
mod metadata;
mod peer;
mod piece_picker;
mod session;
mod storage;
mod torrent;
mod types;

pub use session::{Session, SessionError};
pub use storage::Storage;
pub use types::{SessionConfig, SessionStats, TorrentId, TorrentState, TorrentSummary};
