# AGENTS.md - BitTorrent Client

> Guide for AI agents working on this codebase

## Project Overview

This is a BitTorrent client written in Rust, implementing BEP 3 (core protocol), BEP 5 (DHT), BEP 9 (magnet links), BEP 10 (extensions), BEP 15 (UDP tracker), and BEP 23 (compact peers). It's a workspace-based project with multiple crates.

Architecture: Headless daemon (`cmd/`) with multiple frontends (`tui/`, CLI planned).

## Core Crates

### 1. bittorrent-core

**Responsibility**: Core torrent session management, peer handling, and download coordination.

**Key Entry Points**:
- `Session` / `SessionBuilder` - Main API for controlling the daemon
- `SessionConfig` - Configuration (listen addr, DHT, paths)
- `TorrentId` (= `InfoHash`) - Stable torrent identifier
- `StorageBackend` trait - Pluggable storage (FS, memory, test)

**Architecture Pattern**: Actor model
- `SessionManager` runs in its own async task
- Each `Torrent` runs as a separate task
- Each `PeerConnection` runs as a separate task
- `StorageManager` handles disk I/O in a task
- Communication via mpsc channels
- State shared via `Arc<RwLock<_>>`

**Key Modules**:
| Module | Purpose |
|--------|---------|
| `session.rs` | Main session manager, torrent lifecycle |
| `torrent.rs` | Per-torrent state machine, piece management |
| `peer/` | Peer connection handling, wire protocol |
| `storage/` | Disk I/O abstraction with backends |
| `protocol/` | BitTorrent wire protocol codec |
| `piece_picker.rs` | Rare-first piece selection strategy |
| `choker.rs` | Round-robin choking/unchoking |
| `bitfield.rs` | Piece bitmap with serde support |
| `metadata.rs` | Magnet link metadata exchange (BEP 9) |
| `events/` | Event bus for UI/monitoring |

**Important Design Decisions**:
- Heat-based block request tracking to prevent over-requesting
- Separate piece picker managing availability and block state
- EMA-based rate estimation for progress tracking
- Optional `turmoil` dependency for network simulation

**Common Tasks**:
- Adding new peer protocol extensions → modify `protocol/`
- Changing piece selection → edit `piece_picker.rs`
- Storage customization → implement `StorageBackend`
- UI updates → listen to `EventBus` events

---

### 2. mainline-dht

**Responsibility**: BitTorrent Mainline DHT implementation (BEP 5) for peer discovery without trackers.

**Key Entry Points**:
- `Dht::new(port)` / `Dht::with_config(config)` - Create DHT node
- `bootstrap()` - Join DHT network via bootstrap nodes
- `get_peers(info_hash)` - Stream peers for a torrent
- `announce_peer(info_hash, port)` - Announce participation
- `NodeId` - 160-bit identifier with XOR distance metric

**Architecture Pattern**: Actor model
- `DhtHandler` is the public handle (Clone, Send)
- `DhtActor` runs the event loop in a spawned task
- Commands sent via mpsc channel
- Responses via oneshot or streaming mpsc

**Key Modules**:
| Module | Purpose |
|--------|---------|
| `dht.rs` | Main DHT actor, message loop, protocol logic |
| `routing_table.rs` | K-bucket routing table (K=8 nodes/bucket) |
| `node_id.rs` | 160-bit IDs, XOR distance, BEP 42 secure generation |
| `message.rs` | KRPC protocol messages (bencode over UDP) |
| `peer_store.rs` | LRU cache for announced peers |
| `token.rs` | Rotating secrets for announce validation |
| `transaction.rs` | In-flight request tracking with timeouts |

**BEP 5 Implementation Details**:
- Bootstrap nodes: router.bittorrent.com, dht.transmissionbt.com, router.utorrent.com
- Iterative lookup with parallel queries (alpha=3)
- Token rotation every 5 min, valid for 10 min
- BEP 42: Node ID prefix = CRC32C(masked IP)
- Node status lifecycle: Questionable → Good/Bad

**Common Tasks**:
- Adding DHT queries → extend `message.rs` and `dht.rs`
- Routing table changes → modify `routing_table.rs`
- Security hardening → review `token.rs` and `node_id.rs`

---

### 3. tracker-client

**Responsibility**: HTTP and UDP tracker client for peer discovery via trackers.

**Key Entry Points**:
- `TrackerHandler::new()` - Create tracker client
- `announce(AnnounceParams)` - Generic announce (routes by URL scheme)
- `HttpTrackerClient` - HTTP tracker (BEP 3)
- `UdpTrackerClient` - UDP tracker (BEP 15)
- `AnnounceParams` - Builder-pattern parameters

**Architecture Pattern**: Actor + trait abstraction
- `TrackerHandler` is the public API
- `TrackerManager` runs in background task
- `TrackerClient` trait abstracts HTTP vs UDP
- Scheme-based routing: `http://` → HTTP, `udp://` → UDP

**Key Modules**:
| Module | Purpose |
|--------|---------|
| `client.rs` | Orchestration, `TrackerManager` actor, `TrackerClient` trait |
| `http.rs` | HTTP tracker using `reqwest`, bencode responses |
| `udp.rs` | BEP 15 UDP tracker with state machine |
| `types.rs` | `AnnounceParams`, `TrackerResponse`, `Events` |

**Protocol Details**:

HTTP:
- 15s timeout via `reqwest`
- Compact format (6 bytes/peer): `[u8; 4] IP + u16 BE port`
- Dictionary format: list of `{ip, port}` dicts

UDP (BEP 15):
- Magic protocol ID: `0x41727101980`
- Connection ID cached 60s
- Transaction ID: random i32 for matching
- Exponential backoff: 15 * 2^n seconds, max 8 retries
- State machine: `ConnectSent` → `ConnectReceived` → `AnnounceSent`

**Common Tasks**:
- Adding tracker features → check `scrape()` (currently unimplemented)
- UDP protocol changes → modify `udp.rs` state machine
- HTTP parsing changes → update `http.rs` response handling

---

## Supporting Crates

| Crate | Purpose |
|-------|---------|
| `bencode` | Bencode serialization/deserialization |
| `bittorrent-common` | Shared types: `InfoHash`, `PeerId`, piece hashing |
| `magnet-uri` | Magnet link parsing (BEP 9) |
| `metrics` | Metrics collection and reporting |
| `cmd` | Daemon binary (`btd`) |
| `tui` | Terminal user interface |

## Key Types Shared Across Crates

```rust
// From bittorrent-common
InfoHash  // 20-byte SHA-1 hash, identifies torrents
PeerId    // 20-byte client identifier

// Used throughout
TorrentId = InfoHash  // In bittorrent-core
info_hash: InfoHash   // In mainline-dht for get_peers
info_hash: [u8; 20]   // In tracker-client AnnounceParams
```

## Async Patterns

All crates use **tokio** with these patterns:

1. **Actor Pattern**: Handle + Actor task + mpsc channel
2. **Shared State**: `Arc<RwLock<T>>` or `Arc<Mutex<T>>`
3. **Cancellation**: `tokio::select!` with shutdown signals
4. **Timeouts**: `tokio::time::timeout` for operations

Example actor structure:
```rust
// Public handle
#[derive(Clone)]
pub struct Handler {
    sender: mpsc::Sender<Message>,
}

// Internal actor
struct Actor {
    receiver: mpsc::Receiver<Message>,
    state: SharedState,
}

impl Actor {
    async fn run(mut self) {
        while let Some(msg) = self.receiver.recv().await {
            match msg { /* handle */ }
        }
    }
}
```

## Testing Strategy

- Unit tests in `src/` files alongside code
- Integration tests in `tests/` directories
- `turmoil` for network simulation (optional feature)
- `TrackerHandler::new_noop()` for test mocks

## Common Gotchas

1. **InfoHash serialization**: Always raw bytes, never hex strings in protocols
2. **Port 6881**: Default listen port, but check for conflicts
3. **UDP bind**: May fail if port in use - DHT uses port 0 for ephemeral
4. **BEP 42**: Secure NodeID generation required for some trackers
5. **Piece validation**: SHA-1 of piece data, not including length prefix
6. **Bitfield**: Length = ceil(piece_count / 8), spare bits are zero

## BEP Compliance Summary

| BEP | Status | Crate |
|-----|--------|-------|
| 3 Core Protocol | Full | bittorrent-core |
| 5 DHT | Full | mainline-dht |
| 9 Magnet URI | Full | magnet-uri, bittorrent-core |
| 10 Extensions | Full | bittorrent-core/protocol |
| 15 UDP Tracker | Full | tracker-client |
| 23 Compact Peer | Full | bittorrent-core, tracker-client |

## Dependencies Worth Knowing

| Crate | Used For |
|-------|----------|
| tokio | Async runtime |
| tracing | Structured logging |
| metrics | Metrics collection |
| reqwest | HTTP tracker client |
| crc | BEP 42 CRC32C |
| lru | Peer store caching |
| sha1 | Piece hash verification |
