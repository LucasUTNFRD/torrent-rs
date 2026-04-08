# bittorrent-core Architecture Documentation

## Overview

`bittorrent-core` is the core BitTorrent client implementation providing torrent download, seeding, and peer management functionality. It implements the BitTorrent protocol with support for DHT, tracker-based peer discovery, and magnet links.

## Primary Responsibility

The crate provides a complete BitTorrent client implementation with:
- Session management for multiple concurrent torrents
- Peer connection handling and wire protocol
- Piece selection and download management
- Disk storage abstraction
- DHT and tracker-based peer discovery
- Metadata exchange for magnet links

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         Session                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ TrackerHandler│  │ DhtHandler   │  │   Storage    │          │
│  │  (optional)  │  │  (optional)  │  │  (Backend)   │          │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘          │
│         └─────────────────┴─────────────────┘                   │
│                           │                                     │
│                   ┌───────┴───────┐                             │
│                   ▼               ▼                             │
│           ┌──────────┐    ┌──────────┐                         │
│           │ Torrent  │    │ Torrent  │  ...                     │
│           │   (1)    │    │   (2)    │                         │
│           └────┬─────┘    └────┬─────┘                         │
│                │               │                                │
│         ┌──────┴──────┐ ┌──────┴──────┐                        │
│         ▼             ▼ ▼             ▼                        │
│    ┌─────────┐   ┌─────────┐   ┌─────────┐                     │
│    │ PeerConn│   │ PeerConn│   │ PeerConn│  ...                 │
│    │    1    │   │    2    │   │    3    │                     │
│    └─────────┘   └─────────┘   └─────────┘                     │
│                                                                │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  EventBus (torrent events, peer events, session events)  │  │
│  └─────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Key Modules

### 1. `session.rs` - Entry Point
**Responsibilities:**
- Main public API for the BitTorrent daemon
- Manages multiple torrents concurrently
- Handles incoming TCP connections
- Bootstraps DHT and manages tracker announcements
- Routes commands between client and internal managers

**Key Types:**
- `Session` - Handle to a running session, provides async methods for all operations
- `SessionBuilder` - Builder pattern for session configuration
- `SessionManager` - Internal background task handling commands
- `SessionCommand` - Commands sent from Session to SessionManager
- `SessionError` - Error types for session operations

**Public API:**
```rust
impl Session {
    pub fn new(config: SessionConfig) -> Self
    pub async fn add_torrent(&self, path: impl AsRef<Path>) -> Result<InfoHash, SessionError>
    pub async fn add_magnet(&self, uri: impl AsRef<str>) -> Result<InfoHash, SessionError>
    pub async fn remove_torrent(&self, id: InfoHash) -> Result<(), SessionError>
    pub async fn shutdown(&self) -> Result<(), SessionError>
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent>
    pub async fn subscribe_torrent(&self, id: InfoHash) -> Result<watch::Receiver<TorrentProgress>, SessionError>
}
```

### 2. `torrent.rs` - Per-Torrent State Machine
**Responsibilities:**
- Manages peer connections for a single torrent
- Handles piece management and download progress
- Processes incoming/outgoing peer messages
- Coordinates tracker announcements
- Manages metadata fetching for magnet links

**Key Types:**
- `Torrent` - Core torrent state machine
- `TorrentMessage` - Messages sent to torrent from peers/session
- `TorrentError` - Error types
- `Pid` - Peer identifier (torrent-local)
- `PeerOrigin` - Source of peer connection (inbound/outbound)
- `Metrics` - Per-torrent metrics (downloaded, uploaded, peers discovered)

**States:**
- Downloading - Actively downloading pieces
- Seeding - Have all pieces, uploading to others
- FetchingMetadata - For magnet links, fetching info dictionary

### 3. `peer/` - Peer Connection Handling
**Responsibilities:**
- Manages individual peer connections
- Implements BitTorrent wire protocol
- Handles handshake, message encoding/decoding
- Manages request pipelining and congestion control
- Supports extension protocol (BEP 10)

**Key Types:**
- `PeerConnection` - Core peer connection state machine
- `PeerHandle` - Handle for sending commands to peer
- `PeerInfo` - Peer connection state (choking, interest, rates)
- `BitfieldState` - Manages peer bitfield lifecycle
- `PeerMessage` - Commands sent to peer tasks
- `ConnectionError` - Peer connection errors

**Features:**
- Request pipelining with congestion control (slow start)
- Extended handshake for metadata exchange
- Automatic re-request on timeout
- Heartbeat keepalive (60s interval)

### 4. `storage/` - Disk I/O Management
**Responsibilities:**
- Abstracts disk I/O operations
- Handles piece verification (SHA1 hash checking)
- Manages file handle caching
- Supports both single-file and multi-file torrents

**Key Types:**
- `StorageBackend` - Async trait for storage implementations
- `DiskStorage` - Default disk-based storage
- `StorageManager` - Background task handling storage operations
- `StorageMessage` - Commands sent to storage manager
- `StorageError` - Storage operation errors

**Operations:**
- `add_torrent` - Register torrent for storage
- `add_seed` - Register existing content for seeding
- `write_piece` - Write verified piece to disk
- `read_block` - Read block for uploading to peers
- `verify_piece` - Verify piece hash

### 5. `piece_picker.rs` - Piece Selection Strategy
**Responsibilities:**
- Implements rarest-first piece selection
- Tracks piece availability across peers
- Manages block-level request state
- Handles endgame mode (completing in-progress pieces first)
- Provides file-level progress tracking

**Key Types:**
- `PieceManager` - Core piece selection logic
- `BlockRequest` - Individual block request identifier
- `BlockMetadata` - Per-block state tracking
- `PieceMetadata` - Per-piece state and availability
- `PieceState` - States: NotRequested, InProgress, Downloaded, Have
- `AvailabilityUpdate` - Bitfield or Have message update

**Selection Strategy:**
1. Prioritize completing pieces already in progress
2. Select rarest pieces (fewest peers have them)
3. Use heat tracking to avoid over-requesting same blocks
4. Heat thresholds allow retrying stalled requests

### 6. `choker.rs` - Choking Algorithm
**Responsibilities:**
- Manages upload slots (unchoked peers)
- Implements round-robin choking for fairness
- Tracks peer interest state

**Key Types:**
- `Choker` - Core choking logic

**Algorithm:**
- Fixed number of upload slots (configurable, default 4)
- Interested peers get slots FIFO
- Periodic re-evaluation rotates slots round-robin
- Disinterested peers free up slots

### 7. `bitfield.rs` - Piece Bitmap
**Responsibilities:**
- Compact representation of available pieces
- Validates bitfield length and spare bits
- Supports iteration over set bits

**Key Types:**
- `Bitfield` - Core bitmap implementation
- `BitfieldError` - Validation errors

**Features:**
- MSB-first bit ordering (BitTorrent spec)
- Validates spare bits in last byte
- Resize capability for magnet links
- Iterator over set piece indices

### 8. `metadata.rs` - Magnet Link Support
**Responsibilities:**
- Manages metadata fetching for magnet URIs
- Implements BEP 9 (Metadata Exchange Extension)
- Validates metadata hash against info hash

**Key Types:**
- `Metadata` - Metadata state (TorrentFile vs MagnetUri)
- `MetadataState` - Pending, Fetching, Complete
- `MetadataPiece` - Individual metadata piece tracking

**Process:**
1. Receive metadata_size from peer extended handshake
2. Request metadata pieces (16KiB each)
3. Reassemble and validate SHA1 hash
4. Parse bencode to construct Info struct

### 9. `protocol/` - Wire Protocol
**Responsibilities:**
- Message encoding/decoding
- Handshake handling
- Extension protocol support

**Submodules:**
- `peer_wire.rs` - Core protocol messages (Message, MessageCodec, Handshake)
- `extension.rs` - Extended messaging (BEP 10), metadata exchange

**Message Types:**
- KeepAlive, Choke, Unchoke, Interested, NotInterested
- Have, Bitfield, Request, Piece, Cancel
- Extended, Port

### 10. `events/` - Event System
**Responsibilities:**
- Broadcast events for UI/monitoring
- Decouples event producers from consumers

**Key Types:**
- `EventBus` - Central event distribution
- `SessionEvent` - Session lifecycle events
- `TorrentEvent` - Torrent state changes
- `PeerEvent` - Peer connection events

### 11. `detail.rs` - Data Transfer Objects
**Responsibilities:**
- Types for TUI/RPC API
- Stable interface for querying torrent state

**Key Types:**
- `TorrentDetail` - Complete torrent info for UI
- `TorrentMeta` - Static torrent metadata
- `PeerSnapshot` - Peer connection state snapshot
- `TrackerStatus` - Tracker announce status
- `FileInfo` - File progress information

### 12. `metrics/` - Progress Tracking
**Responsibilities:**
- Track download/upload progress
- Calculate rates and ETA

**Key Types:**
- `TorrentProgress` - Live progress metrics
- `TorrentState` - High-level torrent states

## Design Patterns

### 1. Actor Pattern
- `SessionManager`, `Torrent`, `PeerConnection`, `StorageManager` each run as separate async tasks
- Communication via message passing (mpsc channels)
- Provides isolation and concurrent operation

### 2. Builder Pattern
- `SessionBuilder` for session configuration
- `SessionConfig` with sensible defaults

### 3. Type Safety
- `InfoHash` = `InfoHash` for stable identification
- `Pid` for per-torrent peer identification
- Strongly typed message enums

### 4. Error Handling
- `thiserror` for error type definitions
- Error propagation via Result types
- Graceful degradation (e.g., DHT optional)

### 5. Resource Management
- `Arc<dyn StorageBackend>` for pluggable storage
- `Arc<RwLock<_>>` for shared state
- Clean shutdown via watch channels

## Public API Exports (from lib.rs)

```rust
// Core session
pub use session::{Session, SessionBuilder, SessionError};
pub use session_config::SessionConfig;

// Types
pub use types::InfoHash;
pub use detail::{
    Direction, FileInfo, PeerSnapshot, TorrentDetail, 
    TorrentMeta, TrackerState, TrackerStatus,
};

// Events
pub use events::SessionEvent;

// Progress
pub use metrics::progress::{TorrentProgress, TorrentState};

// Storage
pub use storage::StorageBackend;
```

## Dependencies

**Core:**
- `tokio` - Async runtime
- `bytes` - Byte buffer handling
- `tokio-util` - Codec support

**Protocol:**
- `bencode` - Bencode encoding/decoding (local crate)
- `bittorrent-common` - Common types (local crate)
- `sha1` - Piece hashing

**Networking:**
- `tracker-client` - Tracker communication (local crate)
- `mainline-dht` - DHT implementation (local crate)
- `magnet-uri` - Magnet link parsing (local crate)

**Utilities:**
- `tracing` - Logging
- `thiserror` - Error handling
- `async-trait` - Async traits
- `directories` - Config directories

**Optional:**
- `turmoil` - Network simulation (sim feature)

## Key Files Summary

| File | Lines | Purpose |
|------|-------|---------|
| session.rs | 946 | Main API, session management |
| torrent.rs | 1579 | Per-torrent state machine |
| peer/peer_connection.rs | 1049 | Peer wire protocol |
| piece_picker.rs | 718 | Piece selection |
| storage/mod.rs | 533 | Storage abstraction |
| metadata.rs | 481 | Magnet link metadata |
| bitfield.rs | 488 | Piece bitmap |
| choker.rs | 322 | Choking algorithm |
| protocol/peer_wire.rs | 459 | Message codec |
| detail.rs | 185 | DTOs for UI |

## Testing Features

The crate includes simulation support via the `sim` feature:
- Uses `turmoil` for deterministic network simulation
- Mock storage backend support
- Direct peer injection bypassing tracker/DHT

## Integration Points

1. **bittorrent-common** - Shared types (InfoHash, PeerID, TorrentInfo)
2. **tracker-client** - HTTP/UDP tracker announces
3. **mainline-dht** - DHT peer discovery
4. **magnet-uri** - Magnet link parsing
5. **bencode** - Bencode serialization

This architecture provides a clean separation of concerns with the session as the central coordinator, torrents managing their own peer sets, and peers handling protocol details independently.
