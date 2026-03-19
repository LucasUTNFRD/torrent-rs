# Parallelized Hash Pool in Libtorrent

Hash verification (SHA-1 for v1, SHA-256 for v2) is one of the most CPU-intensive tasks in libtorrent. To handle high-speed downloads (e.g., 10Gbps+), libtorrent uses a dedicated, parallelized hash pool.

## 1. The Hashing Problem

In older BitTorrent clients, hashing was often performed on the main thread or the single disk thread. This created a significant bottleneck:
- **CPU Bottleneck:** The CPU could not keep up with the network speed.
- **Disk Bottleneck:** The disk thread was blocked waiting for the CPU to finish a hash, meaning it couldn't read or write more data.

## 2. The Solution: `m_hash_threads`

Modern libtorrent (v2.0+) uses a dedicated **`hash_thread_pool`**. This is a specialized pool separate from the general disk I/O threads.

### The Mechanism:
- **Dedicated Pool:** The `mmap_disk_io` struct (in `src/mmap_disk_io.cpp`) maintains two separate thread pools: `m_generic_threads` (for metadata, file management, and I/O) and `m_hash_threads` (exclusively for hash calculations).
- **Parallel Pieces:** Multiple pieces can be hashed simultaneously on different CPU cores. This is particularly effective for large files or initial piece checking.

## 3. Implementation Details

### `hasher` Class
The `hasher` and `hasher256` classes (in `src/hasher.cpp`) provide a thin wrapper around cryptographic libraries (like OpenSSL or libgcrypt). These classes are designed to be efficient and thread-safe.

### Hash Job Dispatching
When a piece is fully downloaded and ready for verification:
1. The torrent creates a `hash_job`.
2. The job is queued for the `m_hash_io_jobs` queue.
3. A thread from `m_hash_threads` picks it up and performs the calculation.
4. The result (pass/fail) is returned to the `session_impl` via a callback.

## 4. Performance Tuning

The behavior of the hash pool is controlled by several settings:
- **`hashing_threads`**: Specifies the number of threads dedicated to hashing. Setting this to the number of logical cores can significantly speed up initial checks.
- **`hasher_thread_divisor`**: A dynamic factor (used in `mmap_disk_io`) that can automatically scale hashing resources based on load.

## 5. Summary of Benefits
- **No Blocking:** The main network loop and disk I/O threads are never blocked by CPU-heavy SHA calculations.
- **Multi-Core Scaling:** Throughput scales linearly with the number of available CPU cores.
- **Improved Pipelining:** Libtorrent can be downloading piece $N+1$, writing piece $N$ to disk, and hashing piece $N-1$ all at the same time.
