# Events & Per-Torrent Metrics — Architecture Plan

## Goal

Implement two systems:

1. **Event bus** — typed, categorized events emitted by actors, consumed by TUI, CLI, and tests.
2. **Per-torrent progress** — a `watch`-based snapshot of current torrent state, consumed by anything that needs to display progress.

Developer observability (Prometheus/metrics crate) is explicitly out of scope for this task.

---

## Directory Structure

```
src/
├── events/
│   mod.rs          # re-exports, EventBus struct
│   torrent.rs      # TorrentEvent enum
│   peer.rs         # PeerEvent enum
│   session.rs      # SessionEvent enum (session-wide, not per-torrent)
├── metrics/
│   mod.rs          # re-exports
│   progress.rs     # TorrentProgress struct + impl
```

Do not create a `metrics` module if one already exists for developer metrics. If it does, use a submodule like `metrics/progress.rs` and keep them separated.

---

## Step 1 — Define the Event Types

### `src/events/torrent.rs`

```rust
#[derive(Debug, Clone)]
pub enum TorrentEvent {
    StateChanged     { prev: TorrentState, next: TorrentState },
    TorrentFinished,
    TorrentPaused,
    TorrentResumed,
    FileCompleted    { file_index: u32 },
    TrackerAnnounced { url: String, peers_received: u32 },
    TrackerError     { url: String, error: String, times_in_row: u32 },
    HashFailed       { piece_index: u32 },
    MetadataReceived,
}
```

### `src/events/peer.rs`

```rust
#[derive(Debug, Clone)]
pub enum PeerEvent {
    Connected    { addr: SocketAddr, direction: Direction },
    Disconnected { addr: SocketAddr, reason: DisconnectReason },
    Choked       { addr: SocketAddr },
    Unchoked     { addr: SocketAddr },
    Snubbed      { addr: SocketAddr },
    Banned       { addr: SocketAddr },
}

#[derive(Debug, Clone)]
pub enum Direction { Inbound, Outbound }

#[derive(Debug, Clone)]
pub enum DisconnectReason {
    Timeout,
    ProtocolError,
    Banned,
    SessionShutdown,
    Other(String),
}
```

### `src/events/session.rs`

Session-wide events not tied to a specific torrent (listen socket, external IP discovery, etc.).

```rust
#[derive(Debug, Clone)]
pub enum SessionEvent {
    ListenSucceeded { addr: SocketAddr },
    ListenFailed    { addr: SocketAddr, error: String },
    ExternalIpDiscovered { addr: IpAddr },
}
```

### `src/events/mod.rs`

```rust
pub mod peer;
pub mod session;
pub mod torrent;

pub use peer::PeerEvent;
pub use session::SessionEvent;
pub use torrent::TorrentEvent;
```

---

## Step 2 — Define TorrentProgress

This is the per-torrent snapshot. It is **not** an event — it is current state.

### `src/metrics/progress.rs`

```rust
#[derive(Debug, Clone)]
pub struct TorrentProgress {
    pub name:             String,
    pub total_pieces:     u32,
    pub verified_pieces:  u32,
    pub failed_pieces:    u32,
    pub total_bytes:      u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes:   u64,
    pub connected_peers:  u32,
    pub download_rate:    f64,   // bytes/sec, computed by session on interval
    pub upload_rate:      f64,
    pub state:            TorrentState,
    pub eta_seconds:      Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentState {
    Checking,
    Downloading,
    Seeding,
    Paused,
    Error(String),
    Finished,
}

impl Default for TorrentProgress {
    fn default() -> Self { /* zero values, state = Checking */ }
}
```

---

## Step 3 — Define the EventBus

The `EventBus` is instantiated once per `TorrentSession`. It holds the senders. The session owns it; consumers receive handles.

### `src/events/mod.rs` (addition)

```rust
use tokio::sync::broadcast;

pub const TORRENT_EVENT_CAPACITY: usize = 256;
pub const PEER_EVENT_CAPACITY: usize    = 512;
pub const SESSION_EVENT_CAPACITY: usize = 64;

pub struct EventBus {
    pub torrent_tx: broadcast::Sender<TorrentEvent>,
    pub peer_tx:    broadcast::Sender<PeerEvent>,
    pub session_tx: broadcast::Sender<SessionEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            torrent_tx: broadcast::channel(TORRENT_EVENT_CAPACITY).0,
            peer_tx:    broadcast::channel(PEER_EVENT_CAPACITY).0,
            session_tx: broadcast::channel(SESSION_EVENT_CAPACITY).0,
        }
    }

    pub fn subscribe_torrent(&self) -> broadcast::Receiver<TorrentEvent> {
        self.torrent_tx.subscribe()
    }

    pub fn subscribe_peer(&self) -> broadcast::Receiver<PeerEvent> {
        self.peer_tx.subscribe()
    }

    pub fn subscribe_session(&self) -> broadcast::Receiver<SessionEvent> {
        self.session_tx.subscribe()
    }
}
```

---

## Step 4 — Integrate into TorrentSession

`TorrentSession` owns:
- `event_bus: EventBus` — for emitting events
- `progress_tx: watch::Sender<TorrentProgress>` — for emitting current state

Expose both handles publicly so callers can subscribe before the session starts.

```rust
pub struct TorrentSession {
    // ... existing fields ...
    event_bus:   EventBus,
    progress_tx: watch::Sender<TorrentProgress>,
}

impl TorrentSession {
    pub fn new(/* existing args */) -> Self {
        let (progress_tx, _) = watch::channel(TorrentProgress::default());
        Self {
            // ...
            event_bus: EventBus::new(),
            progress_tx,
        }
    }

    /// Call this before spawning the session task.
    pub fn subscribe_torrent_events(&self) -> broadcast::Receiver<TorrentEvent> {
        self.event_bus.subscribe_torrent()
    }

    pub fn subscribe_peer_events(&self) -> broadcast::Receiver<PeerEvent> {
        self.event_bus.subscribe_peer()
    }

    pub fn progress(&self) -> watch::Receiver<TorrentProgress> {
        self.progress_tx.subscribe()
    }
}
```

---

## Step 5 — Emit Events from Actor Message Handlers

Inside the session's message handlers, emit events after state transitions. Always carry full context in the event payload — no secondary lookups needed by consumers.

### Pattern

```rust
fn on_piece_verified(&mut self, piece_index: u32, elapsed: Duration) {
    // 1. update internal state
    self.verified_pieces += 1;

    // 2. update progress snapshot
    self.update_progress();

    // 3. emit event if piece failure (piece success is captured in progress, not events)
    // NOTE: piece-level success is high-frequency — do NOT emit TorrentEvent per piece.
    // Only emit for notable state changes (hash failure, torrent finished, etc.)
}

fn on_piece_failed(&mut self, piece_index: u32) {
    self.failed_pieces += 1;
    self.update_progress();
    let _ = self.event_bus.torrent_tx.send(TorrentEvent::HashFailed { piece_index });
}

fn on_peer_connected(&mut self, addr: SocketAddr, direction: Direction) {
    self.connected_peers += 1;
    self.update_progress();
    let _ = self.event_bus.peer_tx.send(PeerEvent::Connected { addr, direction });
}

fn on_torrent_finished(&mut self) {
    self.update_progress_state(TorrentState::Finished);
    let _ = self.event_bus.torrent_tx.send(TorrentEvent::TorrentFinished);
}
```

### `update_progress` helper

```rust
fn update_progress(&self) {
    let snapshot = TorrentProgress {
        total_pieces:     self.total_pieces,
        verified_pieces:  self.verified_pieces,
        // ... map all fields
    };
    // send() on watch never fails (receiver may be gone, that's fine)
    let _ = self.progress_tx.send(snapshot);
}
```

Call `update_progress()` at the end of every handler that mutates state visible to the user. Batch if needed — one call per handler is fine, per-block is not.

---

## Step 6 — Consuming in Tests

Create receivers **before** constructing or spawning the session. This is the critical ordering constraint with `broadcast`.

```rust
#[tokio::test]
async fn test_peer_connected_event() {
    let session = TorrentSession::new(/* ... */);

    // Subscribe BEFORE spawning
    let mut peer_rx = session.subscribe_peer_events();
    let progress_rx = session.progress();

    tokio::spawn(async move { session.run().await });

    // Trigger condition that causes peer connection
    // ...

    // Assert event
    let event = tokio::time::timeout(
        Duration::from_secs(1),
        peer_rx.recv()
    ).await
    .expect("timeout waiting for PeerEvent")
    .expect("channel closed");

    assert!(matches!(event, PeerEvent::Connected { .. }));
}
```

For deterministic assertions where broadcast lag is a concern, wrap recv() in a timeout. Never assert on event ordering across categories — `TorrentEvent` and `PeerEvent` channels are independent.

---

## Step 7 — Consuming in CLI/TUI

### CLI (progress bar via `indicatif`)

```rust
let progress_rx = session.progress();

tokio::spawn(async move {
    let pb = ProgressBar::new(100);
    loop {
        let p = progress_rx.borrow_and_update().clone();
        let pct = (p.verified_pieces as f64 / p.total_pieces as f64 * 100.0) as u64;
        pb.set_position(pct);
        pb.set_message(format!(
            "{} peers | ↓{:.1} KB/s",
            p.connected_peers,
            p.download_rate / 1024.0
        ));
        if p.state == TorrentState::Finished { pb.finish(); break; }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
});
```

### TUI (Ratatui)

The TUI render loop reads `progress_rx.borrow()` for the progress panel and drains `peer_rx` / `torrent_rx` into an in-memory `VecDeque<String>` for the event log panel. The event log is display-only — events are never persisted.

```rust
// In TUI state
struct AppState {
    progress:   watch::Receiver<TorrentProgress>,
    peer_rx:    broadcast::Receiver<PeerEvent>,
    torrent_rx: broadcast::Receiver<TorrentEvent>,
    event_log:  VecDeque<String>,  // capped at e.g. 200 entries
}

// In render loop tick
fn tick(&mut self) {
    // drain available events into log (non-blocking)
    while let Ok(ev) = self.peer_rx.try_recv() {
        self.event_log.push_back(format!("{:?}", ev));
        if self.event_log.len() > 200 { self.event_log.pop_front(); }
    }
    while let Ok(ev) = self.torrent_rx.try_recv() {
        self.event_log.push_back(format!("{:?}", ev));
        if self.event_log.len() > 200 { self.event_log.pop_front(); }
    }
}
```

---

## Implementation Order

1. Create `src/events/` module with all enums — no integration yet, just types.
2. Create `src/metrics/progress.rs` with `TorrentProgress` and `TorrentState`.
3. Add `EventBus` to `src/events/mod.rs`.
4. Add `event_bus` and `progress_tx` fields to `TorrentSession` — no emission yet.
5. Add `subscribe_*` and `progress()` accessor methods to `TorrentSession`.
6. Wire `update_progress()` helper and call it from existing handlers.
7. Add event emissions to handlers one actor at a time: PeerConnections first, then TorrentSession, then Tracker.
8. Write integration tests for at least: peer connected, peer disconnected, torrent finished, hash failed.
9. Wire progress receiver to CLI progress bar.
10. Wire both receivers to TUI state (event log + progress panel) — only if TUI work is in scope for this task.

---

## Constraints and Invariants Claude Code Must Respect

- **Never emit `TorrentEvent` or `PeerEvent` per block or per piece download** — only for state transitions and notable failures.
- **`update_progress()` must be called after every handler that mutates user-visible state** — but at most once per handler, not per sub-operation within a handler.
- **Test receivers must be created before the session is spawned** — document this constraint with a comment at each test site.
- **`send()` return values on broadcast must be ignored with `let _ =`** — a lagging or absent receiver is not an error.
- **`TorrentProgress` must be `Clone` and contain no `Arc`, `Mutex`, or nested locks** — it is a plain value snapshot.
- **`DisconnectReason::Other(String)` exists for forward compatibility** — do not use it for known reasons that should be their own variant.
- **Do not add session-wide events to `TorrentEvent`** — session-level things (listen socket, external IP) go in `SessionEvent`.
