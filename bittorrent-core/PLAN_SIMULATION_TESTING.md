changes

# BitTorrent Core: Deterministic Simulation Plan

This document outlines the testing strategy for the `bittorrent-core` Rust implementation. These tests utilize `tokio::turmoil` for deterministic, discrete-event simulation of the actor-based architecture.

## 1. Core Protocol & Data Integrity (`tests/transfer.rs`)
**Replicates:** `test_transfer.cpp`, `test_checking.cpp`, `test_v2.cpp`

### 1.1. Basic E2E Transfer (Happy Path)
*   **Goal**: Verify a complete 1:1 transfer between a seeder and a downloader.
*   **Logic**:
    *   Spawn `Seeder` with complete data on `10.0.0.1`.
    *   Spawn `Downloader` with empty storage on `10.0.0.2`.
    *   Connect them and wait for `TorrentStatus::Finished`.
*   **Assertions**:
    *   Downloader reaches `Seeding` state.
    *   SHA-1/SHA-256 hashes of the downloaded files match the source.

### 1.2. Piece Corruption Recovery
*   **Goal**: Ensure the client detects and repairs corrupted data on disk.
*   **Logic**:
    *   Download 50% of a torrent.
    *   Shutdown `Session`.
    *   Manually overwrite a block in the virtual disk with random bytes.
    *   Restart `Session` and call `force_recheck()`.
*   **Assertions**:
    *   `Torrent` actor emits `HashFailedAlert`.
    *   Progress drops to exclude the corrupted piece.
    *   Client successfully re-downloads the piece and finishes.

### 1.3. Resume Data Persistence
*   **Goal**: Verify that the client doesn't re-download data it already has.
*   **Logic**:
    *   Download 3 pieces.
    *   Save "Fast Resume" data.
    *   Wipe the `Session` but keep the files.
    *   Restart using the Resume data.
*   **Assertions**:
    *   Client starts at 3 pieces, not 0.
    *   No network requests are sent for the first 3 pieces.

---

## 2. Wire Protocol & Peer Actor (`tests/peer_wire.rs`)
**Replicates:** `test_peer_connection.cpp`, `test_pe_crypto.cpp`

### 2.1. Handshake & Type-State Logic
*   **Goal**: Test the `New -> Handshaking -> Connected` transition.
*   **Logic**:
    *   Host A (Client) waits for connection.
    *   Host B (Mock) sends raw bytes: `BitTorrent protocol` + `InfoHash`.
*   **Assertions**:
    *   Client responds with its own Handshake + PeerID.
    *   Connection drops if `InfoHash` is wrong.

### 2.2. Protocol Violation Handling
*   **Goal**: Robustness against malicious or buggy peers.
*   **Logic**:
    *   Send a message with a 1GB length prefix.
    *   Send a `Have` message for a piece index out of bounds.
*   **Assertions**:
    *   `Peer` actor drops the connection immediately.
    *   `Session` actor remains stable (no panics).

---

## 3. Swarm Orchestration & Choking (`tests/swarm.rs`)
**Replicates:** `test_swarm.cpp`, `test_optimistic_unchoke.cpp`

### 3.1. Tit-for-Tat (Unchoke Slots)
*   **Goal**: Verify the unchoking algorithm rewards high-speed uploaders.
*   **Logic**:
    *   Client has 2 unchoke slots.
    *   Connect 4 peers: Peer A (Fast), Peer B (Medium), Peer C (Slow), Peer D (Idle).
*   **Assertions**:
    *   Client unchokes A and B.
    *   Every 30s (simulated), Client "Optimistically Unchokes" D to test speed.

### 3.2. End-Game Mode
*   **Goal**: Minimize time to finish the last piece.
*   **Logic**:
    *   Torrent has 1 block left.
*   **Assertions**:
    *   `Torrent` actor sends `Request` to *all* available peers.
    *   Once block arrives from Peer X, `Cancel` is sent to Peers Y and Z.

---

## 4. Discovery & Routing (`tests/discovery.rs`)
**Replicates:** `test_dht.cpp`, `test_tracker.cpp`, `test_metadata_extension.rs`

### 4.1. DHT Bootstrap
*   **Goal**: Build a routing table from a single entry point.
*   **Logic**:
    *   Node A (Bootstrap) is running.
    *   Nodes B..Z join sequentially, each pointing to Node A.
*   **Assertions**:
    *   Routing tables populate according to XOR distance.
    *   `get_peers` query successfully finds nodes in distant buckets.

### 4.2. Magnet Link (Metadata Exchange)
*   **Goal**: Download `.torrent` file from a peer via BEP 9.
*   **Logic**:
    *   Downloader has only `InfoHash` (Magnet).
    *   Seeder has full Metadata.
*   **Assertions**:
    *   Downloader requests and receives metadata pieces.
    *   `Torrent` actor initializes the full file tree once metadata is validated.

---

## 5. Resilience & Fault Injection (`tests/faults.rs`)
**Replicates:** `test_timeout.cpp`, `test_error_handling.cpp`

### 5.1. Network Timeout
*   **Goal**: Peer Actor drops unresponsive connections.
*   **Logic**:
    *   Set `sim.topology().link_latency()` to 10s.
    *   Peer completes handshake then goes silent.
*   **Assertions**:
    *   Connection is closed after `SessionConfig::peer_timeout`.

### 5.2. Disk Failure
*   **Goal**: Handle I/O errors gracefully.
*   **Logic**:
    *   Inject `EIO` (I/O Error) into the `Storage` actor's `Write` operation.
*   **Assertions**:
    *   `Torrent` actor enters `Error` state.
    *   Download stops; UI/Alerts show the specific disk error.

---

## Simulation Implementation Guidelines

1.  **Determinism**: Always use `sim.run().await` which advances time only when all actors are idle.
2.  **Seeding**: Initialize `StdRng::seed_from_u64(42)` in every test.
3.  **Isolation**: Use Turmoil's `Sim::host` to ensure each node has its own virtual networking stack.
4.  **Mocking**: Use a trait-based `Storage` provider to allow injecting `MockStorage` for Phase 5 tests.

