# Disk I/O Mechanism in Libtorrent

Libtorrent uses a sophisticated, multi-threaded disk I/O subsystem designed to maximize throughput and minimize latency, especially on high-speed connections.

## 1. Disk I/O Architecture

Rather than a single "disk pool," libtorrent employs a **Task-based Thread Pool** architecture. The core of this system is the `disk_io_thread_pool` and the `disk_interface`.

### Key Components:
- **`disk_io_thread_pool`**: A dynamic thread pool that manages worker threads. It scales the number of threads based on the number of queued jobs and system settings (`settings_pack::max_threads`).
- **`disk_job`**: Every disk operation (read, write, hash, etc.) is encapsulated as a "job" and queued for the pool.
- **`disk_interface`**: An abstraction that allows for different disk I/O backends (e.g., POSIX, Memory Mapped, or even "Disabled" for testing).

## 2. Memory Management (The Buffer Pool)

Libtorrent manages disk buffers through a specialized **`disk_buffer_pool`**. This is not a simple "pool" but a sophisticated memory manager that:
- **Pre-allocates buffers**: To avoid frequent `malloc`/`free` calls.
- **Enforces limits**: It monitors total memory usage and can trigger "eviction" or "choking" of peers if disk buffers exceed the configured cache size.
- **Reference Counting**: Buffers are often reference-counted (via `disk_buffer_holder`) to ensure they are not freed while a network operation (like an `async_write`) is still using them.

## 3. Performance Backends

### Mmap Disk I/O (`mmap_disk_io`)
On modern systems, libtorrent often defaults to or prefers Memory Mapped files. This bypasses much of the overhead of traditional `read()`/`write()` system calls:
- The OS manages the file cache (Page Cache) directly.
- It reduces the number of data copies between kernel space and user space.

### Job Fencing (`disk_job_fence`)
To ensure data integrity, the disk subsystem uses a "fence" mechanism. If a job requires exclusive access to a file or piece (like a `write` followed by a `read`), the fence ensures that subsequent jobs are queued and only released once the critical operation completes.

## 4. Summary of Workflow (Block vs. Piece)

BitTorrent protocol divides a **piece** into multiple **blocks** (typically 16 KiB). Libtorrent handles them as follows:

1.  **Block Arrival:** A `peer_connection` receives a single block of data.
2.  **Immediate Write:** It does **not** wait for the whole piece. Instead, it immediately:
    - Requests a buffer from the `disk_buffer_pool`.
    - Creates a `write_job` for that specific block.
    - Pushes it to the `disk_io_thread_pool`.
    - This keeps memory usage low because the client doesn't need to buffer several MiB of piece data in RAM.
3.  **Tracking Completion:** The `piece_picker` tracks which blocks have been written.
4.  **Piece Verification (Hashing):** Once the **last block** of a piece is written to disk:
    - The `torrent` object triggers `verify_piece()`.
    - A `hash_job` is created for the **entire piece**.
    - This job is sent to the `m_hash_threads` pool (see `HASH_POOL.md`).
5. **The "Write-Then-Verify" Strategy**

A common point of confusion is why libtorrent writes data to disk *before* verifying the hash. This "Write-Then-Verify" architecture is a deliberate design choice for high performance.

### Why not buffer in RAM?
If libtorrent waited to receive an entire **piece** (which can be 32 MiB or larger) before writing it, the application's RAM usage would explode when downloading hundreds of torrents. By writing each **16 KiB block** immediately:
- **Constant Memory Footprint:** RAM usage remains low and predictable regardless of piece size.
- **Kernel-Level Buffering:** libtorrent offloads the "buffering" task to the **Operating System Page Cache**.

### How Verification Works
Once the last block of a piece is written, libtorrent triggers a hash check:
1.  **The "Read-Back":** The `hash_thread_pool` issues a read for the entire piece.
2.  **RAM-to-RAM Speed:** Since the blocks were just written, the data is almost certainly still in the **OS Page Cache (RAM)**. The "read" operation usually doesn't touch the physical disk; it's a lightning-fast memory copy.
3.  **The `HAVE` Protection:** Even though the data is on "disk" (or in the OS cache), libtorrent **never** advertises that it has the piece to other peers until the hash check passes. If the hash fails, the "bad" data is eventually overwritten by correct blocks.

## 6. Low-Level System Calls (POSIX)

For the `posix_disk_io` backend, libtorrent communicates with the operating system using standard POSIX calls, but with specific configurations for performance and safety:

### The `pwrite` Syscall
Unlike the standard `write()`, libtorrent uses **`::pwrite()`** (in `src/file.cpp`).
- **Atomic Seek + Write:** `pwrite` takes an explicit file offset as an argument.
- **Thread Safety:** This allows multiple disk threads to write to the same file descriptor simultaneously without interfering with each other's "file cursor" (the shared seek position).
- **Looping (`pwrite_all`):** libtorrent wraps this in a `pwrite_all` loop to handle "short writes" (where the OS only accepts part of a buffer) ensuring the full 16 KiB block is delivered.

### File Opening Flags
Files are opened using **`::open()`** with several important flags (see `src/file.cpp`):
- **`O_RDWR | O_CREAT`**: Standard read/write access.
- **`O_CLOEXEC`**: Prevents the file descriptor from being leaked to child processes.
- **`O_NOATIME`**: (Best effort) Tells the OS not to update the "Last Accessed" timestamp, reducing unnecessary disk metadata writes.
- **`O_SYNC` (Mapped to `open_mode::no_cache`)**: If the user enables "Write Through" mode, libtorrent uses `O_SYNC` to ensure the OS doesn't return from the write call until the data is physically on the disk.

### Why not `writev`?
While libtorrent uses vectored I/O (`writev` / `chained_buffer`) for the **network** (sending data to peers), it generally uses standard `pwrite` for the **disk**. This is because disk writes are typically single 16 KiB blocks, so the overhead of setting up a scatter-gather iovec for a single buffer provides no performance benefit.

