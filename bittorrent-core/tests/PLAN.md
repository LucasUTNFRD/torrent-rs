# Specification: libtorrent Transfer Simulation (Rust Port)

This document describes the architecture and logic of the `transfer_sim` and `test_disk_io` components in `libtorrent`, intended for porting to a Rust-based torrent engine.

## 1. Core Concept: Deterministic Virtualization
The simulation relies on **deterministic data generation** rather than real file I/O. This allows for multi-gigabyte transfer tests without consuming disk space or memory, and ensures that every byte transferred can be validated against a known seed.

### 1.1. Data Generation Engine
The fundamental unit is a **16 KiB block**.
- **Block ID**: `(piece_index << 8) | (block_index & 0xFF)`.
- **Fill Pattern**: Each block is filled with its Block ID (4-byte integer) repeated throughout the 16,384 bytes.
- **V1 Hashes (SHA-1)**: Computed by hashing the generated blocks for a piece.
- **V2 Hashes (SHA-256)**: Each block has its own hash; these form the leaves of a Merkle tree per file.

## 2. Detailed Mock: `test_disk_io`
The `test_disk_io` struct implements the disk interface by acting as a stateful virtual volume.

### 2.1. Internal State
- **`m_have` (Bitfield)**: A bitfield where each bit represents one 16 KiB block. This tracks which parts of the "virtual disk" have been "written."
- **`m_pad_bytes`**: A map tracking padding required for v2 torrents (where files are padded to piece boundaries).
- **`m_event_queue`**: A queue of `(time_point, callback)` pairs used to simulate asynchronous I/O completion.
- **`m_write_queue`**: A counter to simulate backpressure (high/low watermarks).

### 2.2. Async Operations Logic

#### `async_write(piece, offset, buffer)`
1. **Validation**: Before "writing," the mock compares the incoming `buffer` against the deterministic generator (`generate_block_fill`).
2. **State Update**: If the data is correct, it sets the corresponding bit in `m_have`.
3. **Latency**: Calculates `seek_time` + `write_time` and pushes the completion callback to the event queue.
4. **Error Injection**: If `space_left` is less than block size, it returns `no_space_on_device`.

#### `async_read(piece, offset, length)`
1. **Bit Check**: Checks `m_have` for the requested block.
2. **Data Synthesis**:
   - If the bit is set: Returns a buffer filled via `generate_block_fill`.
   - If `corrupt_data_in` counter is triggered: Returns randomized bytes instead.
3. **Latency**: Calculates `seek_time` + `read_time`.

#### `async_hash(piece, flags)`
1. **Verification**: Checks if **all** blocks for the piece have their bits set in `m_have`.
2. **Hash Return**:
   - If all blocks exist: Generates the SHA-1 (and optionally SHA-256 block hashes) and returns them.
   - If any block is missing: Returns a zeroed or randomized hash to trigger a hash failure in the session.

### 2.3. The Latency Model
The mock simulates hardware characteristics to test timeout logic:
- **`seek_time`**: Applied only if the current operation's `(piece, offset)` is not immediately following the previous operation.
- **`hash_time`**: Applied per block during hashing operations.
- **`read/write_time`**: Fixed overhead per 16 KiB block.

## 3. Network Simulation Environment
The simulation environment (`run_test`) provides a virtualized network stack.

### 3.1. Topology
- **Virtual Nodes**: Each `Session` is assigned a virtual IP (e.g., `50.0.0.1`).
- **SOCKS Proxy**: A virtual node acting as a SOCKS5 server to test proxy traversal.
- **HTTP Server**: A mock web server for **Web Seeding**. It uses the same `generate_block_fill` logic to serve ranges of files via HTTP GET.

### 3.2. Lifecycle
1. **Setup**: Instantiate two sessions (Downloader and Seeder) with custom `test_disk_io` configurations.
2. **Torrent Add**:
   - Seeder adds torrent with `existing_files_mode::full_valid` (all bits in `m_have` start as `1`).
   - Downloader adds torrent with `no_files` (all bits start as `0`).
3. **Connection**: Downloader is instructed to `connect_peer` to the Seeder's virtual IP.
4. **Sim Run**: The event loop advances virtual time until the Downloader reaches 100%.
5. **Teardown**: Verify that `Downloader.m_have` is entirely `1`s and that metadata matches.

## 4. Rust Porting Guidelines

### 4.1. Data Generation (Pure Functions)
```rust
fn generate_block(piece: u32, block: u32) -> Vec<u8> {
    let val = (piece << 8) | (block & 0xFF);
    let bytes = val.to_ne_bytes();
    let mut buffer = Vec::with_capacity(16384);
    for _ in 0..4096 {
        buffer.extend_from_slice(&bytes);
    }
    buffer
}
```

### 4.2. Storage Trait Mock
Implement your storage trait (e.g., `AsyncRead + AsyncWrite`) using a `struct` containing:
- `bitfield: BitVec`
- `last_offset: u64`
- `config: TestDiskConfig` (latencies, corruption triggers)

### 4.3. Matrix Testing
Use `proptest` or similar to replicate `run_matrix_test`. Iterate through:
- **Versions**: v1, v2, Hybrid.
- **Piece Sizes**: 16K (small), 32K (normal), 64K (large).
- **Transport**: TCP, uTP, SOCKS5, HTTP (WebSeed).
- **Files**: Single file vs. Multiple files (testing padding logic).

## 5. Flags Matrix
| Flag | Description |
| :--- | :--- |
| `tx::v2_only` | Use SHA-256 Merkle trees; no SHA-1. |
| `tx::magnet` | Download metadata first via DHT/PEX before starting data transfer. |
| `tx::corruption` | Seeder sends valid data for X pieces, then switches to random noise. |
| `tx::resume_restart` | Stop at 50%, save resume data, destroy session, restart. |
| `tx::web_seed` | Download from HTTP server instead of BitTorrent peer. |
