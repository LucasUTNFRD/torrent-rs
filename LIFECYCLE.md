# Peer Connection Lifecycle and Retry Mechanism

This document provides a detailed analysis of how libtorrent manages peer connections, including the retry mechanism, error filtering, and timeout handling.

## Table of Contents

1. [Connection Lifecycle Overview](#connection-lifecycle-overview)
2. [Retry Mechanism](#retry-mechanism)
3. [Error Filtering and Classification](#error-filtering-and-classification)
4. [Timeout Mechanisms](#timeout-mechanisms)
5. [Fast Reconnect](#fast-reconnect)
6. [Configuration Settings](#configuration-settings)

---

## Connection Lifecycle Overview

### Connection Establishment

**Incoming Connections** (`session_impl.cpp:2947`)
- Validated against filters, limits, and paused state
- `bt_peer_connection` object created and tracked in `m_connections`
- `start()` called to begin handshake

**Outgoing Connections** (`torrent.cpp:7508`)
- Protocol selection (uTP/TCP) based on peer history
- Socket created and `async_connect()` initiated
- `on_connection_complete()` handles success/failure (`peer_connection.cpp:6316`)

### Connection States

```
INITIAL → CONNECTING → CONNECTED → HANDSHAKING → ACTIVE → DISCONNECTING → CLOSED
```

**Key State Variables:**
- `m_connecting`: Set during connection establishment (outgoing only)
- `m_connected`: Set after successful TCP connection
- `m_disconnecting`: Set during disconnect process
- `m_failed`: Set for failed connections (affects retry logic)

---

## Retry Mechanism

### Core Retry Algorithm

The retry mechanism is implemented in `peer_list::find_connect_candidates()` (`peer_list.cpp:518`).

**Retry Delay Formula:**
```cpp
delay = (failcount + 1) * min_reconnect_time
```

Where:
- `failcount`: Number of consecutive failed connection attempts (0-31, 5-bit value)
- `min_reconnect_time`: Base reconnect timeout (default: 60 seconds)

**Implementation Details** (`peer_list.cpp:580-583`):
```cpp
if (pe.last_connected
    && session_time - pe.last_connected <
    (int(pe.failcount) + 1) * state->min_reconnect_time)
    continue;
```

### Failcount Management

**Incrementing Failcount** (`peer_list.cpp:477`):
```cpp
void peer_list::inc_failcount(torrent_peer* p)
{
    // failcount is a 5 bit value
    if (p->failcount == 31) return;
    
    bool const was_conn_cand = is_connect_candidate(*p);
    ++p->failcount;
    if (was_conn_cand && !is_connect_candidate(*p))
        update_connect_candidates(-1);
}
```

**Connection Closure Handling** (`peer_list.cpp:1274`):
```cpp
void peer_list::connection_closed(const peer_connection_interface& c, ...)
{
    // Update last_connected timestamp
    if (!c.fast_reconnect())
        p->last_connected = std::uint16_t(session_time);
    
    // Increment failcount on failure
    if (c.failed())
    {
        if (p->failcount < 31) ++p->failcount;
    }
    
    // Re-add to connect candidates if eligible
    if (is_connect_candidate(*p))
        update_connect_candidates(1);
}
```

**Reset on Success** (`torrent.cpp:11393`):
```cpp
void torrent::clear_failcount(torrent_peer* p)
{
    m_peer_list->set_failcount(p, 0);
}
```

### Connect Candidate Eligibility

**Eligibility Check** (`peer_list.cpp:503`):
```cpp
bool peer_list::is_connect_candidate(torrent_peer const& p) const
{
    if (p.connection
        || p.banned
        || p.web_seed
        || !p.connectable
        || (p.seed && m_finished)
        || int(p.failcount) >= m_max_failcount)  // Failcount check
        return false;
    return true;
}
```

Peers with `failcount >= max_failcount` are permanently excluded from connection attempts until the failcount is reset (e.g., by successful connection from peer source).

---

## Error Filtering and Classification

### Disconnect Severity Levels

Defined in `peer_connection_interface.hpp:49-56`:

```cpp
using disconnect_severity_t = aux::strong_typedef<std::uint8_t, struct disconnect_severity_tag>;

static constexpr disconnect_severity_t normal{0};     // Clean disconnection
static constexpr disconnect_severity_t failure{1};    // Connection failure (increments failcount)
static constexpr disconnect_severity_t peer_error{2}; // Protocol/peer error (increments failcount)
```

### When Failcount is Incremented

**Critical Code Path** (`peer_connection.cpp:4334-4337`):
```cpp
if (error > normal)
{
    m_failed = true;
}
```

The `m_failed` flag is set when `error > normal` (i.e., `failure` or `peer_error`), which causes `peer_list::connection_closed()` to increment the failcount.

### Error Classification

**Retry-Triggering Errors (severity = failure/peer_error):**

| Error | Code | Description |
|-------|------|-------------|
| `timed_out` | errors::timed_out | General connection timeout |
| `timed_out_no_handshake` | errors::timed_out_no_handshake | Handshake timeout |
| `timed_out_inactivity` | errors::timed_out_inactivity | Inactivity timeout |
| `timed_out_no_request` | errors::timed_out_no_request | No request timeout |
| `timed_out_no_interest` | errors::timed_out_no_interest | No interest timeout |
| `connection_refused` | error::connection_refused | Connection refused by peer |
| `connection_reset` | error::connection_reset | Connection reset by peer |
| `connection_aborted` | error::connection_aborted | Connection aborted |
| `host_unreachable` | error::host_unreachable | Host unreachable |
| `no_memory` | errors::no_memory | Out of memory |
| `too_many_connections` | errors::too_many_connections | Connection limit reached |
| Protocol errors | Various | Invalid messages, encryption failures |

**Non-Retry Errors (severity = normal):**

| Error | Code | Description |
|-------|------|-------------|
| `self_connection` | errors::self_connection | Connected to self (bans peer) |
| `torrent_aborted` | errors::torrent_aborted | Torrent aborted |
| `torrent_paused` | errors::torrent_paused | Torrent paused |
| `upload_upload_connection` | errors::upload_upload_connection | Both upload-only |
| `uninteresting_upload_peer` | errors::uninteresting_upload_peer | Uninteresting peer |
| `optimistic_disconnect` | errors::optimistic_disconnect | Optimistic disconnect |
| `eof` | error::eof | Clean EOF |

**Special Handling for Self-Connections** (`peer_connection.cpp:4441-4443`):
```cpp
if (ec == errors::self_connection && m_peer_info && t)
    t->ban_peer(m_peer_info);
```

### uTP to TCP Fallback

**Automatic Retry on uTP Failure** (`peer_connection.cpp:4221-4253`):

When a uTP connection fails:
1. Mark peer as not supporting uTP: `m_peer_info->supports_utp = false`
2. Trigger fast reconnect: `fast_reconnect(true)`
3. Disconnect with `normal` severity (no failcount increment)
4. Schedule immediate TCP reconnection via `post()`

This is a **graceful retry** that doesn't count against the failcount.

### Holepunch Mode

**NAT Traversal Retry** (`peer_connection.cpp:4255-4269`):

When in holepunch mode:
```cpp
if (m_holepunch_mode)
    fast_reconnect(true);
```

This allows immediate retry through alternative paths without incrementing failcount.

---

## Timeout Mechanisms

### Connection Establishment Timeout

**Implementation** (`peer_connection.cpp:4944-4967`):

```cpp
if (m_connecting)
{
    int connect_timeout = m_settings.get_int(settings_pack::peer_connect_timeout);
    if (m_peer_info) connect_timeout += 3 * m_peer_info->failcount;
    
    // SSL handshakes are slow
    if (is_ssl(m_socket))
        connect_timeout += 10;
    
    // I2P handshakes are slower
    if (is_i2p(m_socket))
        connect_timeout += 20;
    
    if (d > seconds(connect_timeout)
        && can_disconnect(errors::timed_out))
    {
        connect_failed(errors::timed_out);
        return;
    }
}
```

**Timeout Calculation:**
- Base: `peer_connect_timeout` (default: 10 seconds)
- Penalty: `+ 3 * failcount` seconds per previous failure
- SSL: `+ 10` seconds
- I2P: `+ 20` seconds

**Example:**
- First attempt: 10 seconds
- Second attempt: 13 seconds
- Third attempt: 16 seconds
- With SSL: +10 seconds to each

### Inactivity Timeout

**Implementation** (`peer_connection.cpp:4982-4990`):

```cpp
bool const reading_socket = bool(m_channel_state[download_channel] & peer_info::bw_network);

if (reading_socket && d > seconds(timeout()) && !m_connecting && m_reading_bytes == 0
    && can_disconnect(errors::timed_out_inactivity))
{
    disconnect(errors::timed_out_inactivity, operation_t::bittorrent);
    return;
}
```

**Timeout Value** (`peer_connection.cpp:254-266`):
```cpp
int peer_connection::timeout() const
{
    int ret = m_settings.get_int(settings_pack::peer_timeout);
    if (m_peer_info && m_peer_info->is_i2p_addr)
    {
        // quadruple the timeout for i2p peers
        ret *= 4;
    }
    return ret;
}
```

Default: `peer_timeout` = 120 seconds (480s for I2P)

**Important:** Timeout only enforced when `bw_network` flag is set (actively reading from socket). If rate-limited, timeout is not enforced.

### Handshake Timeout

**Implementation** (`peer_connection.cpp:4993-5009`):

```cpp
int timeout = m_settings.get_int(settings_pack::handshake_timeout);
timeout *= is_i2p(m_socket) ? 4 : 1;

if (reading_socket
    && !m_connecting
    && in_handshake()
    && d > seconds(timeout))
{
    disconnect(errors::timed_out_no_handshake, operation_t::bittorrent);
    return;
}
```

Default: `handshake_timeout` = 10 seconds (40s for I2P)

### Request Timeout (Upload)

**Implementation** (`peer_connection.cpp:5011-5035`):

Disconnects peers that were unchoked but didn't send requests:
```cpp
if (reading_socket
    && !m_connecting
    && m_requests.empty()
    && m_reading_bytes == 0
    && !m_choked
    && m_peer_interested
    && t && t->is_upload_only()
    && d > seconds(60)
    && can_disconnect(errors::timed_out_no_request))
{
    disconnect(errors::timed_out_no_request, operation_t::bittorrent);
    return;
}
```

Hardcoded: 60 seconds for upload-only seeds

### Mutual Interest Timeout

**Implementation** (`peer_connection.cpp:5037-5072`):

Disconnects peers with no mutual interest when connection slots are full:
```cpp
time_duration const time_limit = seconds(
    m_settings.get_int(settings_pack::inactivity_timeout));

if (reading_socket
    && !m_interesting
    && !m_peer_interested
    && d1 > time_limit
    && d2 > time_limit
    && (max_session_conns || max_torrent_conns)
    && can_disconnect(errors::timed_out_no_interest))
{
    disconnect(errors::timed_out_no_interest, operation_t::bittorrent);
    return;
}
```

Default: `inactivity_timeout` = 600 seconds (10 minutes)

Only triggered when:
- Session connections >= `connections_limit - 5`
- OR torrent connections >= `max_connections - 5`

### Piece Request Timeout

**Implementation** (`peer_connection.cpp:4568-4591`):

```cpp
int peer_connection::request_timeout() const
{
    const int deviation = m_request_time.avg_deviation();
    const int avg = m_request_time.mean();
    
    if (m_request_time.num_samples() < 2)
        return m_settings.get_int(settings_pack::request_timeout);
    
    ret = avg + deviation * 2;
    return std::max(ret, m_settings.get_int(settings_pack::request_timeout));
}
```

Default: `request_timeout` = 60 seconds

Uses exponential moving average of request times with 2 standard deviations.

### Block Timeout (Snubbing)

**Implementation** (`peer_connection.cpp:5133-5158`):

```cpp
int const piece_timeout = m_settings.get_int(settings_pack::piece_timeout);

if (m_download_queue.size() > 0
    && m_quota[download_channel] > 0
    && now - m_last_piece.get(m_connect) > seconds(piece_timeout))
{
    // Snub peer - mark all pending blocks as timed_out
    for (auto& qe : m_download_queue)
    {
        if (!qe.timed_out && !qe.not_wanted)
        {
            qe.timed_out = true;
            // Post block_timeout_alert
        }
    }
    snub_peer();
}
```

Default: `piece_timeout` = 20 seconds

---

## Fast Reconnect

### Purpose

Fast reconnect allows immediate retry bypassing normal exponential backoff delays. Used for:
1. uTP to TCP fallback
2. Holepunch mode
3. Encryption negotiation failure

### Implementation

**Fast Reconnect Function** (`peer_connection.cpp:964-978`):
```cpp
void peer_connection::fast_reconnect(bool r)
{
    if (!peer_info_struct() || peer_info_struct()->fast_reconnects > 1)
        return;
    
    m_fast_reconnect = r;
    peer_info_struct()->last_connected = std::uint16_t(m_ses.session_time());
    
    // Rewind timestamp to bypass retry delay
    int const rewind = m_settings.get_int(settings_pack::min_reconnect_time)
        * m_settings.get_int(settings_pack::max_failcount);
    
    if (int(peer_info_struct()->last_connected) < rewind)
        peer_info_struct()->last_connected = 0;
    else
        peer_info_struct()->last_connected -= std::uint16_t(rewind);
    
    if (peer_info_struct()->fast_reconnects < 15)
        ++peer_info_struct()->fast_reconnects;
}
```

**Effect:**
- Sets `last_connected` to current time
- Rewinds by `min_reconnect_time * max_failcount` (e.g., 60s * 3 = 180s)
- This makes the peer immediately eligible for reconnection
- Limits: Maximum 2 fast reconnects per peer (`fast_reconnects > 1` check)

**Timestamp Handling** (`peer_list.cpp:1305-1309`):
```cpp
// If fast reconnect is true, we won't update the timestamp,
// and it will remain the time when we initiated the connection.
if (!c.fast_reconnect())
    p->last_connected = std::uint16_t(session_time);
```

This preserves the rewound timestamp, allowing immediate retry.

---

## Configuration Settings

### Retry-Related Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `max_failcount` | 3 | Maximum failed connection attempts before stopping retries |
| `min_reconnect_time` | 60 seconds | Base reconnect delay |
| `connection_speed` | 200/sec | Outgoing connection rate limit |

### Timeout Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `peer_connect_timeout` | 10 seconds | TCP connection timeout |
| `handshake_timeout` | 10 seconds | BitTorrent handshake timeout |
| `peer_timeout` | 120 seconds | General inactivity timeout |
| `inactivity_timeout` | 600 seconds | Mutual non-interest timeout |
| `request_timeout` | 60 seconds | Piece request timeout base |
| `piece_timeout` | 20 seconds | Block/piece download timeout |

### Behavior Notes

1. **Failcount Penalty on Timeout:** Each connection failure increases the next connection timeout by 3 seconds (`peer_connect_timeout + 3 * failcount`).

2. **I2P Multipliers:** I2P connections have 4x timeouts for handshake and peer timeout due to slower network characteristics.

3. **SSL Handshake Allowance:** SSL connections get +10 seconds for handshake completion.

4. **Rate Limiting Interaction:** Timeouts are not enforced when the peer is rate-limited (bw_network flag not set).

5. **Connection Slot Pressure:** Mutual interest timeout only triggers when connections are near the limit.

6. **Fast Reconnect Limits:** Maximum 2 fast reconnects per peer to prevent abuse.

---

## Summary

The libtorrent retry mechanism uses:

1. **Exponential backoff** via `(failcount + 1) * min_reconnect_time`
2. **Failcount tracking** (0-31) to limit retries per peer
3. **Error severity classification** to determine if retry should increment failcount
4. **Smart timeouts** that adapt to protocol (uTP/TCP), network (I2P), and encryption (SSL)
5. **Fast reconnect** for protocol fallbacks without failcount penalty
6. **Graceful degradation** with longer timeouts for slower networks

The system balances aggressive retry for transient failures with protection against persistent unreachable peers through the failcount mechanism.
