# Specification: libtorrent Transfer and Swarm Simulation (Rust Port)

This document describes the architecture and logic of the simulation framework in `libtorrent`, covering both point-to-point transfers and multi-node swarms.

## 1. Core Concept: Deterministic Virtualization
The simulation relies on **deterministic data generation** rather than real file I/O. This allows for multi-gigabyte transfer tests without consuming disk space or memory, and ensures that every byte transferred can be validated against a known seed.

### 1.1. Data Generation Engine
The fundamental unit is a **16 KiB block**.
- **Block ID**: `(piece_index << 8) | (block_index & 0xFF)`.
- **Fill Pattern**: Each block is filled with its Block ID (4-byte integer) repeated throughout the 16,384 bytes.
- **V1 Hashes (SHA-1)**: Computed by hashing the generated blocks for a piece.
- **V2 Hashes (SHA-256)**: Each block has its own hash; these form the leaves of a Merkle tree per file.

---

## 2. Detailed Mock: `test_disk_io`
The `test_disk_io` struct implements the disk interface by acting as a stateful virtual volume.

### 2.1. Internal State
- **`m_have` (Bitfield)**: A bitfield where each bit represents one 16 KiB block. This tracks which parts of the "virtual disk" have been "written."
- **`m_pad_bytes`**: A map tracking padding required for v2 torrents (where files are padded to piece boundaries).
- **`m_event_queue`**: A queue of `(time_point, callback)` pairs used to simulate asynchronous I/O completion.
- **`m_write_queue`**: A counter to simulate backpressure (high/low watermarks).

---

## 3. Network Simulation Environment

### 3.1. Topology
- **Virtual Nodes**: Each `Session` has a virtual IP (e.g., `50.0.0.x`).
- **Proxies/Servers**: SOCKS4/5 and HTTP (WebSeed) servers are mocked as virtual nodes.

### 3.2. DSL Latency and Rate Model (`dsl_config`)
In swarm tests, network characteristics are often determined by the IP address:
- **Rate Calculation**: `(last_digit_of_IP + 4) * 5` KB/s. 
- **Latency Calculation**: `Rate / 2` ms.
- **Queueing**: Each route has an associated `queue` to simulate DSL modem buffers (default 200,000 bytes) and packet latency.

---

## 4. Swarm Simulation Framework (`setup_swarm`)

This section details the exact mechanics used to create a swarm.

### 4.1. The "Star Topology" Isolation
By default, `setup_swarm` creates a **Star Topology** where Node 0 is the center. 
- **Node 0 (Test Subject)**: No IP filters. It can talk to anyone.
- **Nodes 1..N (Peers)**: An `ip_filter` is applied:
    1.  **Block All**: `0.0.0.0` - `255.255.255.255` is set to `blocked`.
    2.  **Allow Node 0**: Only `50.0.0.1` (Node 0) is unblocked.
- **Result**: Peers 1..N cannot talk to each other. This ensures that every byte downloaded by Node 0 comes directly from a controlled source, and every byte uploaded by Node 0 is tracked without peer-to-peer gossip interference (unless PEX is specifically enabled).

### 4.2. Connection Logic
When the simulation starts:
1.  **Torrent Add**: All nodes `async_add_torrent`.
2.  **Auto-Connect**: Inside Node 0's `on_alert` handler, when `add_torrent_alert` is received:
    - Node 0 loops from `k = 1` to `num_nodes`.
    - It calls `h.connect_peer(endpoint("50.0.x.x", 6881))`.
3.  **Peer IDs**: Each node generates a random `peer_id` to prevent self-connection rejections.

### 4.3. Tick Loop & Termination
- A timer fires every **1 virtual second**.
- **Termination Callback**: `terminate(tick_count, node0_session)` is called. If it returns `true`, the simulation stops.
- **Seeding Timeout**: In `upload` tests, if `tick > 88 * (num_nodes - 1)`, the test fails with "seeding failed!".
- **Auto-Stop (Upload)**: If all nodes 1..N become seeds, the simulation shuts down automatically.

---

## 5. Scenario Deep Dives (`test_swarm.cpp`)

### 5.1. Protocol State Scenarios

#### Seed Mode (`seed_mode`)
- **Setup**: `type = upload`, `num_nodes = 3`.
- **Config**: `params.flags |= torrent_flags::seed_mode`.
- **Dynamics**: Node 0 (the seeder) starts without hashing its files. It assumes they are valid.
- **Assertion**: Verifies that Node 0 can successfully upload to Peers 1 and 2 without a pre-hash check delay.

#### Graceful Pause (`stop_start_download_graceful`)
- **Setup**: `type = download`, `num_nodes = 3`.
- **Dynamics**:
    1.  Downloader (Node 0) reaches 50% completion (`total_wanted_done > total_wanted / 2`).
    2.  `h.pause(graceful_pause)` is called.
    3.  **Verification**: Engine must finish pending requests but stop sending new ones.
    4.  **Resumption**: Upon `torrent_paused_alert`, `h.resume()` is called.
- **Assertion**: The torrent must eventually reach 100% (`is_seed` returns true).

#### Seed Mode + Suggest (`seed_mode_suggest`)
- **Config**: `settings_pack::suggest_mode = suggest_read_cache`.
- **Dynamics**: Node 0 (Seeder) sends `SUGGEST` messages to Peers.
- **Verification**: In `on_alert`, check `peer_log_alert` for outgoing `SUGGEST` messages.

### 5.2. Network Condition Scenarios

#### Dead Peers (`dead_peers`)
- **Network Config**: `timeout_config` overrides `incoming_route` and `outgoing_route`.
    - Incoming Latency: 10 seconds.
    - Outgoing Latency: 5 seconds.
- **Session Config**: `settings_pack::peer_connect_timeout = 1` second.
- **Dynamics**: Node 0 is given 3 non-existent IP addresses (`66.66.66.60-62`).
- **Assertion**: `on_alert` must catch exactly 3 `peer_disconnected_alert` with `error == timed_out`.

#### NAT & Self-Connect (`self_connect`)
- **Network Config**: `nat_config` implements a NAT hop.
    - If target IP is `50.0.0.1`, the route appends a `nat` object that rewrites the source to `51.51.51.51`.
- **Dynamics**: Node 0 is told to connect to its own IP (`50.0.0.1`).
- **Assertion**: The engine must detect the connection originated from itself (via Peer ID or handshake) and disconnect with `errors::self_connection`.

#### DSL Heterogeneity (`dsl_config`)
- **Dynamics**: A swarm of 10 nodes.
- **IP Assignment**: `50.0.0.1` through `50.0.0.10`.
- **Behavior**: Peer 10 has a higher rate and lower latency than Peer 2.
- **Verification**: Tests the piece picker's ability to prefer faster peers in a swarm.

### 5.3. Advanced Logic Scenarios

#### Peer Exchange (`pex`)
- **Setup**: Manual (non-helper) setup of 3 nodes.
    - Node 0 (Downloader)
    - Node 1 (Downloader)
    - Node 2 (Seeder)
- **Topology**:
    - Node 0 connects *only* to Node 1.
    - Node 1 and Node 2 connect to each other.
- **Dynamics**: Node 1 learns about Node 2 from the seeder, then gossips Node 2's IP to Node 0 via PEX.
- **Assertion**: Node 0 receives an `incoming_connection_alert` or `peer_connect_alert` from `50.0.0.3` (Node 2).

#### Session Stats Tracking (`session_stats`)
- **Config**: Periodically call `ses.post_session_stats()`.
- **Assertion**: Check `session_stats_alert` counters.
    - `ses.num_downloading_torrents` must be 1.
    - `ses.num_incoming_extended` must track PEX/Metadata extension handshakes.

#### Delete Files/Partfile (`delete_files`)
- **Setup**: `swarm_test::real_disk`.
- **Dynamics**: Torrent reaches 100%, then `ses.remove_torrent(h, delete_files)`.
- **Assertion**: Uses `stat_file` to verify the "temporary" file is physically removed from the filesystem (`no_such_file_or_directory`).

---

## 6. Matrix Testing (`run_matrix_test`)
Exhaustive combinatorial testing across:
- **Piece Sizes**: 16K, 64K, and non-power-of-2 (v1).
- **Initial States**:
    - `no_files`: Clean download.
    - `full_invalid`: Overwriting corrupt data.
    - `partial_valid`: Resuming with some correct pieces.
- **Combinations**: {v1, v2, Hybrid} x {Magnet, Torrent} x {WebSeed, BT} x {Valid, Corrupt}.

---

## 7. Rust Porting Guidelines (Swarm Specific)

### 7.1. Swarm Coordinator
Implement a `Swarm` struct that manages a collection of `Peer` instances. Advance virtual time manually.

### 7.2. DSL Route Mocking
In Rust, wrap your `TcpStream`/`UdpSocket` in a layer that:
1.  Calculates a delay based on the peer's IP.
2.  Buffers bytes in a `VecDeque` or similar, only "releasing" them to the engine when the virtual clock matches `arrival_time`.

### 7.3. Deterministic Randomness
`libtorrent` seeds its random engine with `0x23563a7f` before each matrix run. Ensure your Rust implementation uses a seedable RNG (like `rand_chacha`) to maintain determinism across test runs.
