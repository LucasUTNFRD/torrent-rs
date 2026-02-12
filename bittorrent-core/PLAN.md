# ChokeManager Implementation Plan

## Overview

Implement a `ChokeManager` that determines which peers to unchoke based on upload/download rates, following the BitTorrent choking algorithm as implemented in libtorrent.

## Core Concepts

The choker is responsible for:
- Deciding which peers receive upload bandwidth (unchoked)
- Which peers are denied upload bandwidth (choked)
- Managing upload slots dynamically
- Tracking peer performance metrics

## Algorithm Support

### 1. Round-Robin (Default)
- Prioritizes peers based on download rate
- Rotates peers that have completed their upload quota
- Ensures fair distribution of upload slots

### 2. Fastest Upload
- Prioritizes peers we're uploading to fastest
- Simple rate-based selection

### 3. Anti-Leech
- Based on "Improving BitTorrent: A Simple Approach" paper
- Prefers peers with few pieces (starters) and peers close to completion
- Uses a V-shaped scoring function

### 4. Rate-Based Choker
- Dynamically adjusts number of upload slots based on upload rates
- Increases threshold by 2 KiB/s per peer
- Balances slot count vs. upload rate saturation

## Implementation Phases

### Phase 1: Core Data Structures

1. **Create `ChokeManager` struct**
   - Upload slots limit (configurable)
   - Unchoke interval (e.g., 10 seconds)
   - Current choking algorithm selection
   - List of managed peers

2. **Extend `PeerConnection`**
   - Add fields:
     - `uploaded_in_last_round: u64`
     - `downloaded_in_last_round: u64`
     - `uploaded_since_unchoked: u64`
     - `time_of_last_unchoke: Instant`
     - `is_choked: bool`
     - `is_interested: bool` (from remote peer)
     - `am_interested: bool` (our interest in peer)
   - Add methods:
     - `get_priority() -> i32`
     - `num_have_pieces() -> usize`

3. **Create `PeerStats` or use existing statistics**
   - Track per-peer upload/download rates
   - Reset counters each round

### Phase 2: Rate Tracking

1. **Track upload rates in `handle_peer_cmd`**
   - When sending `Message::Piece`, record bytes sent
   - Update `uploaded_in_last_round` counter
   - Update `uploaded_since_unchoked` counter

2. **Track download rates**
   - When receiving `Message::Piece`, record bytes received
   - Update `downloaded_in_last_round` counter

3. **Periodic rate reset**
   - At each unchoke interval, reset round counters
   - Preserve `uploaded_since_unchoked` until peer is choked

### Phase 3: Interest Management

1. **Update `PeerConnection`**
   - Implement `send_interested()` method
   - Implement `send_not_interested()` method
   - Handle incoming `Interested`/`NotInterested` messages

2. **Integrate with `Torrent`**
   - `PeerConnection` sends `TorrentMessage::Interested` or `TorrentMessage::NotInterested` to torrent
   - Torrent notifies choker of peer interest changes

3. **Decision logic**
   - Send `Interested` when peer has pieces we want
   - Send `NotInterested` when peer has no useful pieces

### Phase 4: Comparison Functions

Implement comparison functions for peer sorting:

1. **`compare_peers(a, b) -> Ordering`**
   - Compare by priority (torrent priority)
   - Compare by downloaded bytes in last round
   - Return ordering result

2. **`unchoke_compare_rr(a, b, pieces_quota) -> Ordering`**
   - Use `compare_peers()` as primary sort
   - Check if peers completed upload quota
   - Compare upload rates
   - Fall back to `time_of_last_unchoke` (FIFO)

3. **`unchoke_compare_fastest_upload(a, b) -> Ordering`**
   - Use `compare_peers()` as primary sort
   - Compare `uploaded_in_last_round`
   - Fall back to `time_of_last_unchoke`

4. **`anti_leech_score(peer) -> i32`**
   - Calculate V-shaped score based on pieces completed
   - Lower score = more desirable to unchoke
   - Formula: `abs((have_size - total_size/2) * 2000 / total_size)`

5. **`unchoke_compare_anti_leech(a, b) -> Ordering`**
   - Use `compare_peers()` as primary sort
   - Compare anti-leech scores
   - Fall back to `time_of_last_unchoke`

6. **`upload_rate_compare(a, b) -> Ordering`**
   - Compare by upload rate * torrent priority

### Phase 5: Unchoke Algorithm

Implement the main `unchoke_sort()` function:

1. **Rate-based slot calculation** (optional)
   - Sort peers by upload rate
   - Count peers above threshold
   - Increase threshold by 2 KiB/s per peer
   - Set upload_slots dynamically

2. **Sort eligible peers**
   - Filter interested, unchoked peers
   - Use `nth_element` (Rust: `select_nth_unstable`) to find top N peers
   - N = min(upload_slots, peers.len())

3. **Apply unchoke decisions**
   - Unchoke top N peers
   - Choke remaining peers

4. **Update peer states**
   - Set `is_choked` flag
   - Update `time_of_last_unchoke` for newly unchoked peers
   - Send `Choke`/`Unchoke` messages

### Phase 6: Integration

1. **Wire up to `Torrent`**
   - Add `ChokeManager` field to `Torrent`
   - Periodically call unchoke routine (every 10-15 seconds)

2. **Message handling**
   - Handle `Choke`/`Unchoke` received from peers
   - Stop/request pieces accordingly

3. **Configuration**
   - Expose settings:
     - `upload_slots_limit: i32` (-1 = unlimited)
     - `choking_algorithm: enum { RoundRobin, FastestUpload, AntiLeech, RateBased }`
     - `seeding_piece_quota: i32`
     - `unchoke_interval: Duration`

## Data Structures

```rust
pub enum ChokingAlgorithm {
    RoundRobin,
    FastestUpload,
    AntiLeech,
    RateBased,
}

pub struct ChokeManager {
    upload_slots_limit: i32,
    choking_algorithm: ChokingAlgorithm,
    seeding_piece_quota: i32,
    unchoke_interval: Duration,
    last_unchoke: Instant,
    peers: Vec<PeerConnection>,
}

pub struct PeerConnection {
    // existing fields...
    
    // Choking state
    pub is_choked: bool,
    pub am_choking: bool,
    pub is_interested: bool,     // peer is interested in us
    pub am_interested: bool,     // we are interested in peer
    
    // Rate tracking
    pub uploaded_in_last_round: u64,
    pub downloaded_in_last_round: u64,
    pub uploaded_since_unchoked: u64,
    pub time_of_last_unchoke: Option<Instant>,
    
    // Torrent association
    pub torrent_priority: i32,
}
```

## Key Methods

```rust
impl ChokeManager {
    pub fn new(config: &Config) -> Self;
    
    /// Call periodically to recalculate unchoke set
    pub fn unchoke_peers(&mut self);
    
    /// Add a peer to management
    pub fn add_peer(&mut self, peer: PeerConnection);
    
    /// Remove a peer from management
    pub fn remove_peer(&mut self, peer_id: &PeerId);
    
    /// Update peer's uploaded bytes (call when sending Piece)
    pub fn record_upload(&mut self, peer_id: &PeerId, bytes: u64);
    
    /// Update peer's downloaded bytes (call when receiving Piece)
    pub fn record_download(&mut self, peer_id: &PeerId, bytes: u64);
}

impl PeerConnection {
    /// Send Interested message to peer
    pub fn send_interested(&mut self);
    
    /// Send NotInterested message to peer
    pub fn send_not_interested(&mut self);
    
    /// Send Choke message to peer
    pub fn send_choke(&mut self);
    
    /// Send Unchoke message to peer
    pub fn send_unchoke(&mut self);
}
```

## Testing Strategy

1. **Unit tests for comparison functions**
   - Test each comparison with various peer states
   - Verify sorting produces expected order

2. **Integration tests**
   - Mock peer connections
   - Verify correct peers are unchoked
   - Verify rate tracking accuracy

3. **Edge cases**
   - All peers choked
   - No interested peers
   - Rate-based slot calculation
   - Anti-leech scoring

## References

- libtorrent choking implementation (provided C++ code)
- BitTorrent Protocol Specification
- "Improving BitTorrent: A Simple Approach" paper (anti-leech algorithm)
