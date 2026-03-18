# BitTorrent Client Metrics Implementation Plan

## Overview

Implement performance metrics for a Rust BitTorrent client, inspired by libtorrent's `performance_counters.hpp`, using the `metrics` crate for idiomatic Rust design.

## Goals

1. Add observability to peer connections, disk I/O, piece picker, DHT, and uTP
2. Enable performance profiling and bottleneck identification
3. Export metrics to Prometheus for monitoring
4. Maintain compatibility with existing benchmark tools if needed

## Dependencies

```toml
[dependencies]
metrics = "0.21"
metrics-exporter-prometheus = "0.13"
```

## Implementation

### Phase 1: Core Metrics Module

**File**: `src/metrics/mod.rs`

Create module with submodules for each subsystem:

```
src/metrics/
├── mod.rs           # Main module, exports
├── picker.rs        # Piece picker metrics
├── disk.rs          # Disk I/O metrics  
├── net.rs           # Peer connection metrics
├── bt.rs            # BitTorrent protocol metrics
├── dht.rs           # DHT metrics
├── utp.rs           # uTP protocol metrics
└── export.rs        # Prometheus exporter setup
```

### Phase 2: Implement Priority Metrics

Start with the most impactful metrics for performance profiling.

#### 2.1 Piece Picker (Highest CPU Impact)

```rust
// src/metrics/picker.rs
use metrics::counter;

pub fn inc_rare_loops() { counter!("picker.rare_loops").increment(1); }
pub fn inc_rand_loops() { counter!("picker.rand_loops").increment(1); }
pub fn inc_rand_start_loops() { counter!("picker.rand_start_loops").increment(1); }
pub fn inc_busy_loops() { counter!("picker.busy_loops").increment(1); }
pub fn inc_partial_loops() { counter!("picker.partial_loops").increment(1); }
pub fn inc_suggest_loops() { counter!("picker.suggest_loops").increment(1); }
pub fn inc_reverse_rare_loops() { counter!("picker.reverse_rare_loops").increment(1); }
pub fn inc_sequential_loops() { counter!("picker.sequential_loops").increment(1); }
```

**Where to call**: In piece picker algorithm when iterating through pieces.

#### 2.2 Disk I/O (Common Bottleneck)

```rust
// src/metrics/disk.rs
use metrics::{counter, gauge, histogram};

pub fn inc_blocks_read(n: u64) { counter!("disk.blocks_read").increment(n); }
pub fn inc_blocks_written(n: u64) { counter!("disk.blocks_written").increment(n); }
pub fn inc_read_ops(n: u64) { counter!("disk.read_ops").increment(n); }
pub fn inc_write_ops(n: u64) { counter!("disk.write_ops").increment(n); }
pub fn record_read_time(ms: u64) { histogram!("disk.read_time_ms").record(ms); }
pub fn record_write_time(ms: u64) { histogram!("disk.write_time_ms").record(ms); }
pub fn record_job_time(ms: u64) { histogram!("disk.job_time_ms").record(ms); }

pub fn set_queued_jobs(n: usize) { gauge!("disk.queued_jobs").set(n as f64); }
pub fn set_running_jobs(n: usize) { gauge!("disk.running_jobs").set(n as f64); }
```

**Where to call**: In disk I/O layer when completing read/write operations.

#### 2.3 Peer Connections

```rust
// src/metrics/net.rs
use metrics::{counter, gauge};

pub fn inc_connected() { gauge!("net.peers_connected").increment(1); }
pub fn dec_connected() { gauge!("net.peers_connected").decrement(1); }
pub fn inc_half_open() { gauge!("net.peers_half_open").increment(1); }
pub fn dec_half_open() { gauge!("net.peers_half_open").decrement(1); }

pub fn inc_connection_attempt() { counter!("net.connection_attempts").increment(1); }
pub fn inc_connect_timeout() { counter!("net.connect_timeouts").increment(1); }
pub fn inc_incoming_connection() { counter!("net.incoming_connections").increment(1); }
pub fn inc_disconnected(reason: &str) { 
    counter!("net.disconnected_peers", "reason" => reason).increment(1); 
}
```

**Where to call**: In peer connection lifecycle (connect, disconnect, state changes).

#### 2.4 BitTorrent Protocol

```rust
// src/metrics/bt.rs
use metrics::counter;

pub fn inc_piece_request() { counter!("bt.piece_requests").increment(1); }
pub fn inc_invalid_piece_request() { counter!("bt.piece_requests_invalid").increment(1); }
pub fn inc_cancelled_request() { counter!("bt.piece_requests_cancelled").increment(1); }
pub fn inc_choked_request() { counter!("bt.piece_requests_choked").increment(1); }

pub fn inc_piece_passed() { counter!("bt.piece_passed").increment(1); }
pub fn inc_piece_failed() { counter!("bt.piece_failed").increment(1); }

pub fn inc_picker_pick(reason: &str) { 
    counter!("bt.picker_picks", "reason" => reason).increment(1); 
}

// Incoming messages
pub fn inc_msg_incoming(typ: &str) { 
    counter!("bt.msg_incoming", "type" => typ).increment(1); 
}

// Outgoing messages  
pub fn inc_msg_outgoing(typ: &str) { 
    counter!("bt.msg_outgoing", "type" => typ).increment(1); 
}
```

**Where to call**: In peer connection message handlers.

### Phase 3: Secondary Metrics

#### 3.1 Bandwidth

```rust
// src/metrics/traffic.rs
use metrics::counter;

pub fn inc_sent_payload(bytes: u64) { counter!("traffic.sent_payload_bytes").increment(bytes); }
pub fn inc_recv_payload(bytes: u64) { counter!("traffic.recv_payload_bytes").increment(bytes); }
pub fn inc_sent_total(bytes: u64) { counter!("traffic.sent_bytes").increment(bytes); }
pub fn inc_recv_total(bytes: u64) { counter!("traffic.recv_bytes").increment(bytes); }
pub fn inc_sent_overhead(bytes: u64) { counter!("traffic.sent_ip_overhead").increment(bytes); }
pub fn inc_recv_overhead(bytes: u64) { counter!("traffic.recv_ip_overhead").increment(bytes); }

pub fn inc_waste(reason: &str) { 
    counter!("traffic.waste_pieces", "reason" => reason).increment(1); 
}
```

#### 3.2 uTP Protocol

```rust
// src/metrics/utp.rs
use metrics::counter;

pub fn inc_packet_loss() { counter!("utp.packet_loss").increment(1); }
pub fn inc_timeout() { counter!("utp.timeout").increment(1); }
pub fn inc_packet_in() { counter!("utp.packets_in").increment(1); }
pub fn inc_packet_out() { counter!("utp.packets_out").increment(1); }
pub fn inc_fast_retransmit() { counter!("utp.fast_retransmit").increment(1); }
pub fn inc_resend() { counter!("utp.packet_resend").increment(1); }

pub fn set_connected(n: usize) { gauge!("utp.connected").set(n as f64); }
pub fn set_idle(n: usize) { gauge!("utp.idle").set(n as f64); }
```

#### 3.3 DHT

```rust
// src/metrics/dht.rs
use metrics::{counter, gauge};

pub fn inc_msg_in() { counter!("dht.messages_in").increment(1); }
pub fn inc_msg_out() { counter!("dht.messages_out").increment(1); }
pub fn inc_msg_dropped() { counter!("dht.messages_dropped").increment(1); }
pub fn inc_invalid(typ: &str) { counter!("dht.invalid", "type" => typ).increment(1); }

pub fn set_nodes(n: usize) { gauge!("dht.nodes").set(n as f64); }
pub fn set_torrents(n: usize) { gauge!("dht.torrents").set(n as f64); }
```

#### 3.4 Network Events (Event Loop)

```rust
// src/metrics/event_loop.rs
use metrics::counter;

pub fn inc_read_wakeup() { counter!("net.wakeup_read").increment(1); }
pub fn inc_write_wakeup() { counter!("net.wakeup_write").increment(1); }
pub fn inc_tick() { counter!("net.wakeup_tick").increment(1); }
pub fn inc_disk() { counter!("net.wakeup_disk").increment(1); }
pub fn inc_accept() { counter!("net.wakeup_accept").increment(1); }
```

### Phase 4: Prometheus Export

**File**: `src/bin/main.rs` or `src/lib.rs`

```rust
use metrics_exporter_prometheus::PrometheusBuilder;

fn init_metrics() {
    PrometheusBuilder::new()
        .with_http_listener("0.0.0.0:9090")
        .with_push_gateway("localhost:9091", "bittorrent")
        .with_push_interval(std::time::Duration::from_secs(10))
        .install()
        .expect("failed to install prometheus exporter");
}
```

### Phase 5: Torrent State Gauges

```rust
// src/metrics/torrent.rs
use metrics::gauge;

pub fn set_checking(n: usize) { gauge!("torrent.checking").set(n as f64); }
pub fn set_downloading(n: usize) { gauge!("torrent.downloading").set(n as f64); }
pub fn set_seeding(n: usize) { gauge!("torrent.seeding").set(n as f64); }
pub fn set_queued(n: usize) { gauge!("torrent.queued").set(n as f64); }
pub fn set_error(n: usize) { gauge!("torrent.error").set(n as f64); }

pub fn set_pieces_have(n: usize) { gauge!("torrent.pieces_have").set(n as f64); }
```

### Phase 6: File Pool Metrics

```rust
// src/metrics/file_pool.rs
use metrics::{counter, gauge};

pub fn inc_pool_hit() { counter!("file_pool.hits").increment(1); }
pub fn inc_pool_miss() { counter!("file_pool.misses").increment(1); }
pub fn set_pool_size(n: usize) { gauge!("file_pool.size").set(n as f64); }
pub fn inc_thread_stall() { counter!("file_pool.thread_stall").increment(1); }
```

## Integration Checklist

- [ ] Add `metrics` crate to Cargo.toml
- [ ] Create `src/metrics/` directory structure
- [ ] Implement picker metrics (8 counters)
- [ ] Implement disk metrics (8 counters + 4 gauges)
- [ ] Implement net metrics (8 counters + 4 gauges)
- [ ] Implement BT protocol metrics (10 counters)
- [ ] Implement traffic metrics (8 counters)
- [ ] Implement uTP metrics (8 counters + 4 gauges)
- [ ] Implement DHT metrics (8 counters + 4 gauges)
- [ ] Implement event loop metrics (6 counters)
- [ ] Implement torrent state metrics (6 gauges)
- [ ] Add Prometheus exporter to main.rs
- [ ] Test with `cargo run` - verify metrics at http://localhost:9090/metrics
- [ ] Add Grafana dashboard (optional)

## Metric Naming Convention

| Prefix | Category |
|--------|----------|
| `picker.` | Piece picker algorithm |
| `disk.` | Disk I/O operations |
| `net.` | Network/connections |
| `bt.` | BitTorrent protocol |
| `traffic.` | Bandwidth/bytes |
| `utp.` | uTP protocol |
| `dht.` | DHT protocol |
| `torrent.` | Torrent state |
| `file_pool.` | File handle pool |

## References

- libtorrent `include/libtorrent/performance_counters.hpp` - original metrics
- libtorrent `tools/parse_session_stats.py` - benchmark analysis
- https://docs.rs/metrics/ - Rust metrics crate docs