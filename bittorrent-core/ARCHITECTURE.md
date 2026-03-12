# BitTorrent Core Architecture

This document describes the actor-based architecture and communication patterns of the `bittorrent-core` client. This serves as a reference for maintaining determinism and ensuring correct peer-to-peer logic.

## 1. Actor Hierarchy

The system follows a hierarchical actor model where each level manages the lifecycle and state of its children.

```text
Session (Root)
├── DhtHandler (Mainline DHT)
├── Storage (Disk Actor) - Now injected via Arc<Dyn StorageBackend>
└── Torrent (Per-InfoHash Actor)
    ├── TrackerHandler (Announce Logic)
    └── Peer (Wire Protocol Actor - One per connection)
```

## 2. Core Components & Responsibilities

### A. Session (`session.rs`)
- **Role**: Entry point and global manager.
- **Responsibilities**:
    - Spawns and manages `Torrent` actors.
    - Listens for incoming TCP connections and dispatches them to the correct `Torrent` based on `info_hash`.
    - Centralized DHT and Disk I/O access.
- **Communication**: Receives `SessionCommand` via an `unbounded_channel`.

### B. Torrent (`torrent.rs`)
- **Role**: The "Brain" of a specific download/upload.
- **Responsibilities**:
    - Tracks piece availability (Bitfield) and progress.
    - Orchestrates `Peer` actors (choking/unchoking, piece requests).
    - Interfaces with `PiecePicker` for selection logic.
    - Manages tracker announces and DHT peer discovery.
- **Communication**: 
    - Receives `TorrentMessage` from `Session`, `Peer`s, and `TrackerHandler`.
    - Sends `PeerMessage` to connected peers.

### C. Peer (`peer/peer_connection.rs`)
- **Role**: BitTorrent Wire Protocol (BEP 3) implementation.
- **Responsibilities**:
    - Handles handshaking, bitfield exchange, and block transfers.
    - Implements a **Type-State Pattern** (`New` -> `Handshaking` -> `Connected`) to ensure protocol safety.
- **Communication**:
    - Talks to the `Torrent` brain via `mpsc::Sender<TorrentMessage>`.
    - Receives commands (e.g., `SendHave`, `Disconnect`) via `mpsc::Receiver<PeerMessage>`.

### D. Storage (`storage/mod.rs`)
- **Role**: Disk I/O abstraction.
- **Responsibilities**:
    - Reading/Writing blocks to files.
    - Validating piece hashes.
- **Implementation**: Runs blocking I/O in `tokio::task::spawn_blocking` to avoid stalling the async runtime.

## 3. Communication Patterns

| Pattern | Usage | Example |
| :--- | :--- | :--- |
| **MPSC** | Command/Event stream | `Peer` -> `Torrent` (Status updates) |
| **Oneshot** | Request/Response | `Session` -> `Torrent` (Get Stats) |
| **Watch** | State broadcasting | `Torrent` -> UI/API (Progress updates) |
| **Shared Arc** | Read-only configuration | `SessionConfig` shared across actors |

## 4. Deterministic Simulation Strategy

To maintain protocol correctness and allow for discrete event simulation:

1. **Network Virtualization**:
   All networking must be abstracted via a `NetworkProvider` trait. Actors should depend on `AsyncRead + AsyncWrite` rather than concrete `TcpStream` types. This allows swapping real sockets for in-memory duplex streams.

2. **Time Control**:
   Avoid `std::time` or `Instant::now()`. Use `tokio::time` utilities which support `pause()` and `advance()`. In simulation mode, the `SimulationHost` controls the virtual clock.

3. **Seeded Entropy**:
   All randomness (Peer IDs, Piece Picking, Choking) must be derived from a seeded PRNG injected at construction.

4. **Quiescence-Based Advancement**:
   In simulation, time advances only when all actors are waiting (idle). This ensures that race conditions are resolved deterministically based on the event queue priority.
