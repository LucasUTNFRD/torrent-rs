# Peer TCP Read/Write Performance in Libtorrent

Libtorrent's networking layer is built on top of `Boost.Asio` and uses several specialized techniques to achieve high performance while maintaining a low memory footprint.

## 1. Outgoing: Gather-Write with `chained_buffer`

To send data as efficiently as possible, libtorrent uses a **`chained_buffer`** (in `src/chained_buffer.cpp`).

### The Mechanism:
- Instead of copying data into a single contiguous buffer, `chained_buffer` maintains a list of multiple disparate buffers.
- **Vectored I/O (iovec)**: When sending data, libtorrent uses `build_iovec()` to create an array of `boost::asio::const_buffer` (which wraps the system `iovec` struct).
- **Gather-Write**: The OS kernel "gathers" data from these different memory locations and sends them as a single continuous stream over the wire. This is much faster than doing multiple small `write()` calls or one large `memcpy()` followed by a `write()`.

## 2. Peer to Disk-Thread Communication

Peers do not perform disk I/O themselves. Instead, they communicate with the disk thread pool asynchronously to avoid blocking the network loop.

### The `disk_buffer` and `disk_buffer_holder`
The **`disk_buffer`** is the "currency" of data exchange in libtorrent. It is a 16 KiB block of memory allocated from the `disk_buffer_pool`.

**`disk_buffer_holder`** is a RAII wrapper (similar to `std::unique_ptr`) that manages the lifecycle of a disk buffer.

### The Send Workflow (Zero-Copy):
1.  **Request:** A peer receives a `REQUEST` message from a remote peer. It sends an `async_read` job to the **Disk I/O thread**.
2.  **Read:** The Disk I/O thread reads the 16 KiB block into a `disk_buffer`.
3.  **Callback:** The Disk thread posts a completion callback (`on_disk_read_complete`) back to the **Network thread**, passing a `disk_buffer_holder`.
4.  **Header Generation:** the `bt_peer_connection` writes the BitTorrent protocol header (`msg_piece`, `index`, `begin`) into a small, local buffer.
5.  **Chaining:** The `chained_buffer` now looks like this:
    - Buffer 1: Protocol Header (small, copied).
    - Buffer 2: Piece Data (`disk_buffer_holder`, **moved, not copied**).
6.  **Network Send:** `async_write_some` is called using `iovec` pointing to both.
7.  **Destruction:** Once the network write is fully complete, the `chained_buffer` pops the buffers. The `disk_buffer_holder` is destroyed, and the 16 KiB buffer is returned to the pool.

**Result:** The piece data was read from disk into a buffer and sent to the network card without ever being copied in user-space.

## 3. Incoming: Dynamic `receive_buffer`

Incoming data is managed by the **`receive_buffer`** (in `src/receive_buffer.cpp`).

### The Mechanism:
- **Elasticity**: The buffer starts small and grows dynamically (`grow()`) as needed to accommodate incoming messages.
- **Watermarking**: It uses a "watermark" to avoid frequent re-allocations if the incoming message rate is fluctuating.
- **Fast Path Parsing**: Once a message header is fully received, the `receive_buffer` provides a `span<char const>` to the message parser, allowing for zero-copy access to the payload.

## 3. Performance Enhancements

### Socket Buffer Size Tuning
Libtorrent dynamically adjusts the kernel's socket send and receive buffers (`SO_SNDBUF` / `SO_RCVBUF`) via `aux::set_socket_buffer_size`. This is crucial for saturating high-bandwidth connections (e.g., 1Gbps+), as the default OS buffers are often too small.

### Protocol Awareness (Message Batching)
The `peer_connection` can batch multiple small protocol messages (like `HAVE` or `REQUEST`) into a single TCP packet to reduce context switching and syscall overhead.

### Zero-Copy for uTP
For its custom UDP-based protocol (**uTP**), libtorrent uses even more aggressive zero-copy techniques, reading data directly into disk buffers whenever possible.

## 4. Summary of Data Flow
1. **Outgoing:**
   - Disk IO thread finishes a read and returns a `disk_buffer`.
   - `peer_connection` appends it to its `m_send_buffer` (`chained_buffer`).
   - `async_write_some` is called with an `iovec` (no copy).
2. **Incoming:**
   - `async_read_some` reads data into the `receive_buffer`.
   - The buffer is parsed into protocol messages.
   - For piece data, the buffer is passed directly to the `disk_io_thread_pool`.
