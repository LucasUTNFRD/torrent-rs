# Design Review: High-Performance Disk & Network I/O

This document summarizes the architectural review of `DISK_IO.md`, `HASH_POOL.md`, and `PEER_TCP.md`, detailing how `torrent-rs` implements the high-performance patterns used in `libtorrent` using Rust and the `tokio` runtime.

## 1. Disk I/O: "Write-Then-Verify" Strategy

### Core Mechanism
- **Immediate Block Writes:** Each 16 KiB block is written to disk as soon as it arrives, rather than buffering entire pieces (which can be 32 MiB+) in RAM.
- **OS Page Cache Utilization:** By writing immediately, we offload buffering to the kernel's Page Cache.
- **Post-Write Hashing:** Once the final block of a piece is written, a "read-back" occurs. Since the data was recently written, this read typically hits the RAM-based Page Cache, making it near-instant.

### Rust Implementation Notes
- **Syscalls:** Use `std::os::unix::fs::FileExt::pwrite_at` for thread-safe writes at specific offsets without shared seek positions.
- **Concurrency:** Disk operations must be offloaded to `tokio::task::spawn_blocking` or a dedicated thread pool to prevent blocking the async reactor.
- **Safety:** Prefer `pwrite` over `mmap` for initial implementation to avoid `SIGBUS` risks during file truncation, though `memmap2` remains an optimization target.

## 2. Parallelized Hash Pool

### Core Mechanism
- **Dedicated Thread Pool:** Hashing (SHA-1/SHA-256) is CPU-bound and isolated from the I/O-bound disk threads to prevent bottlenecks.
- **Pipelining:** The system maintains a pipeline: Downloading Piece $N+1$, Writing Piece $N$, and Hashing Piece $N-1$ simultaneously.

### Rust Implementation Notes
- **Rayon:** Use the `rayon` crate for the hash pool. Its work-stealing scheduler is superior to FIFO queues for CPU-heavy cryptographic tasks.
- **Hardware Acceleration:** Ensure `sha1` and `sha2` crates have SIMD features enabled (AVX2, SHA-NI) via `Cargo.toml`.

## 3. Peer TCP: Zero-Copy & Vectored I/O

### Core Mechanism
- **Vectored I/O (Gather-Write):** Use `iovec` to send protocol headers (from RAM) and piece data (from the disk buffer) in a single syscall without merging them into a new buffer.
- **Reference Counted Buffers:** Use handles that "move" data between the Disk, Network, and Hash boundaries without copying.

### Rust Implementation Notes
- **`bytes` Crate:** 
  - Use `BytesMut` for the `receive_buffer` to allow zero-copy splitting of message payloads.
  - Use `Bytes` for outgoing data; it acts as an atomically reference-counted handle (`Arc<Vec<u8>>`) that returns to the pool when the last reference (e.g., the socket write) is dropped.
- **Vectored Writes:** Utilize `tokio::net::TcpStream::poll_write_vectored` with `std::io::IoSlice`.

## 4. Performance & Reliability Targets

- **Backpressure:** Implement `tokio::sync::Semaphore` on the `DiskJob` queue. If disk latency spikes, the semaphore will naturally "choke" network reads, preventing memory exhaustion.
- **I/O Uring:** Long-term, transition to `tokio-uring` on Linux for true asynchronous I/O, which eliminates the overhead of `spawn_blocking` context switches.
- **Buffer Pooling:** Use a lock-free structure (like `crossbeam-queue`) to manage a pre-allocated pool of 16 KiB buffers to minimize allocator pressure.
