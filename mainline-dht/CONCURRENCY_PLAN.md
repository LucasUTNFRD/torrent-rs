# DHT Concurrency Implementation Plan

## Overview

This plan implements parallel query execution for DHT searches, matching the concurrency model from the reference C implementation (dht.c). The implementation adds support for up to 3 concurrent in-flight queries per search (ALPHA) and completes searches when 8 nodes have successfully responded.

## Goals

1. **Parallel Queries**: Execute up to 3 concurrent queries per search (ALPHA=3)
2. **C-Code Compatibility**: Match search completion criteria (8 successful responses)
3. **Peer Deduplication**: Remove duplicate peer addresses from results
4. **Non-Breaking**: Maintain existing API compatibility
5. **Clean Architecture**: Separate search state management from actor event loop

## Architecture Changes

### 1. New Types and Structures

#### Search State Management

```rust
/// Tracks an active search for peers (similar to C's struct search)
pub struct SearchState {
    /// Target infohash we're searching for
    pub info_hash: InfoHash,
    /// Target as NodeId for distance calculations
    pub target: NodeId,
    /// Candidate nodes to query, ordered by XOR distance
    pub candidates: BinaryHeap<SearchCandidate>,
    /// Number of queries currently in-flight (max 3)
    pub in_flight: usize,
    /// Number of nodes that have successfully responded (stop at 8)
    pub responded: usize,
    /// Unique peers found (deduplicated)
    pub peers_found: HashSet<SocketAddrV4>,
    /// Nodes that responded with tokens (for announce)
    pub nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)>,
    /// Channel to send final result
    pub completion_tx: Option<oneshot::Sender<GetPeersResult>>,
    /// When search started
    pub started_at: Instant,
}

/// Individual candidate node in a search
pub struct SearchCandidate {
    /// Node information
    pub node: CompactNodeInfo,
    /// XOR distance to target
    pub distance: Distance,
    /// Current status
    pub status: CandidateStatus,
}

/// Status of a candidate node in search
pub enum CandidateStatus {
    /// Not yet queried
    Pending,
    /// Query sent, waiting for response (with TX ID)
    Querying(TransactionId),
    /// Successfully responded
    Responded,
    /// Timed out or error
    Failed,
}

/// Manager for all active searches
pub struct SearchManager {
    /// Active searches by infohash
    pub searches: HashMap<InfoHash, SearchState>,
    /// Map transaction ID -> infohash for routing responses
    pub tx_to_search: HashMap<TransactionId, InfoHash>,
}
```

#### Pending Request Tracking

Update `PendingRequest` to track which search a request belongs to:

```rust
pub struct PendingRequest {
    /// Which search this request belongs to
    pub search_id: InfoHash,
    /// When request was sent
    pub sent_at: Instant,
}
```

### 2. Actor State Updates

Add to `DhtActor`:

```rust
pub struct DhtActor {
    // ... existing fields ...
    
    /// Active searches
    pub search_manager: SearchManager,
}
```

### 3. Event Loop Restructuring

The event loop will handle four concurrent branches:

```rust
async fn run(mut self) -> Result<(), DhtError> {
    let mut buf = [0u8; 1500];
    let mut maintenance_interval = interval(Duration::from_secs(15));
    let mut timeout_check_interval = interval(Duration::from_millis(100));
    
    loop {
        tokio::select! {
            // Branch 1: Incoming UDP packets
            result = self.socket.recv_from(&mut buf) => {
                match result {
                    Ok((size, from)) => {
                        self.handle_packet(&buf[..size], from).await;
                    }
                    Err(e) => tracing::warn!("UDP recv error: {}", e),
                }
            }
            
            // Branch 2: Commands from public API
            Some(cmd) = self.command_rx.recv() => {
                self.handle_command(cmd).await;
            }
            
            // Branch 3: Query timeout checking
            _ = timeout_check_interval.tick() => {
                self.check_timeouts().await;
            }
            
            // Branch 4: Periodic maintenance
            _ = maintenance_interval.tick() => {
                self.perform_maintenance().await;
            }
        }
    }
}
```

## Implementation Phases

### Phase 1: Search State Infrastructure

**Files to modify:**
- `src/search.rs` (new file)
- `src/lib.rs` (add module)

**Tasks:**
1. Create `src/search.rs` with:
   - `SearchState` struct
   - `SearchCandidate` struct  
   - `CandidateStatus` enum
   - `SearchManager` struct
   - Constants

2. Add constants:
   ```rust
   /// Maximum in-flight queries per search (like C's DHT_INFLIGHT_QUERIES)
   pub const MAX_INFLIGHT_QUERIES: usize = 3;
   
   /// Target number of responses to complete search (like C code)
   pub const TARGET_RESPONSES: usize = 8;
   
   /// Query timeout duration
   pub const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
   ```

3. Implement `Ord` for `SearchCandidate` (by XOR distance):
   ```rust
   impl Ord for SearchCandidate {
       fn cmp(&self, other: &Self) -> Ordering {
           // Reverse order for BinaryHeap (min-heap)
           other.distance.cmp(&self.distance)
       }
   }
   ```

### Phase 2: Non-Blocking Query System

**Files to modify:**
- `src/lib.rs` (`DhtActor` implementation)

**Tasks:**

1. **Replace `send_and_wait` with `send_query`:**
   ```rust
   async fn send_query(
       &mut self, 
       addr: SocketAddrV4, 
       query: Query, 
       search_id: InfoHash
   ) -> TransactionId {
       let tx_id = self.next_transaction_id();
       let msg = KrpcMessage::new_query(tx_id, self.node_id, query);
       
       // Track in pending with search reference
       self.pending.insert(tx_id, PendingRequest {
           search_id,
           sent_at: Instant::now(),
       });
       
       // Also track in search manager for routing
       self.search_manager.tx_to_search.insert(tx_id, search_id);
       
       // Fire-and-forget UDP send
       if let Err(e) = self.socket.send_to(&msg.to_bytes(), addr).await {
           tracing::warn!("Failed to send to {}: {}", addr, e);
       }
       
       tx_id
   }
   ```

2. **Implement `check_timeouts`:**
   ```rust
   async fn check_timeouts(&mut self) {
       let now = Instant::now();
       let timed_out: Vec<_> = self.pending
           .iter()
           .filter(|(_, req)| now.duration_since(req.sent_at) > QUERY_TIMEOUT)
           .map(|(tx_id, req)| (*tx_id, req.search_id))
           .collect();
           
       for (tx_id, search_id) in timed_out {
           // Remove from pending
           self.pending.remove(&tx_id);
           self.search_manager.tx_to_search.remove(&tx_id);
           
           // Update search state
           if let Some(search) = self.search_manager.searches.get_mut(&search_id) {
               // Find the candidate with this TX ID and mark as Failed
               for candidate in &mut search.candidates {
                   if matches!(candidate.status, CandidateStatus::Querying(id) if id == tx_id) {
                       candidate.status = CandidateStatus::Failed;
                       search.in_flight -= 1;
                       break;
                   }
               }
               
               // Try to advance the search
               self.advance_search(search).await;
               
               // Check if search should complete
               if self.should_complete_search(search) {
                   self.complete_search(&search_id).await;
               }
           }
       }
   }
   ```

3. **Implement `advance_search`:**
   ```rust
   async fn advance_search(&mut self, search: &mut SearchState) {
       let available_slots = MAX_INFLIGHT_QUERIES.saturating_sub(search.in_flight);
       if available_slots == 0 {
           return;
       }
       
       // Get pending candidates
       let to_query: Vec<_> = search.candidates
           .iter()
           .filter(|c| matches!(c.status, CandidateStatus::Pending))
           .take(available_slots)
           .cloned()
           .collect();
           
       for candidate in to_query {
           let tx_id = self.send_query(
               candidate.node.addr,
               Query::GetPeers { 
                   id: self.node_id,
                   info_hash: search.info_hash,
               },
               search.info_hash,
           ).await;
           
           // Update candidate status
           if let Some(c) = search.candidates.iter_mut().find(|c| c.node.node_id == candidate.node.node_id) {
               c.status = CandidateStatus::Querying(tx_id);
               search.in_flight += 1;
           }
       }
   }
   ```

### Phase 3: Response Handling

**Files to modify:**
- `src/lib.rs` (`handle_incoming` and related)

**Tasks:**

1. **Update `handle_incoming` for responses:**
   ```rust
   async fn handle_incoming(&mut self, data: &[u8], from: SocketAddr) {
       let msg = match KrpcMessage::from_bytes(data) {
           Ok(m) => m,
           Err(e) => {
               tracing::debug!("Failed to parse message from {}: {}", from, e);
               return;
           }
       };
       
       match &msg.body {
           MessageBody::Query(query) => {
               self.handle_query(msg, query, from).await;
           }
           MessageBody::Response(_) => {
               self.handle_response(msg, from).await;
           }
           MessageBody::Error { .. } => {
               self.handle_error(msg, from).await;
           }
       }
   }
   ```

2. **Implement `handle_response`:**
   ```rust
   async fn handle_response(&mut self, msg: KrpcMessage, from: SocketAddr) {
       let tx_id = msg.transaction_id;
       
       // Look up which search this belongs to
       let search_id = if let Some(id) = self.search_manager.tx_to_search.remove(&tx_id) {
           id
       } else {
           tracing::debug!("Received response for unknown transaction from {}", from);
           return;
       };
       
       // Remove from pending
       self.pending.remove(&tx_id);
       
       // Get the search
       let search = if let Some(s) = self.search_manager.searches.get_mut(&search_id) {
           s
       } else {
           return;
       };
       
       // Update search state
       search.in_flight -= 1;
       search.responded += 1;
       
       // Mark candidate as responded
       for candidate in &mut search.candidates {
           if matches!(candidate.status, CandidateStatus::Querying(id) if id == tx_id) {
               candidate.status = CandidateStatus::Responded;
               break;
           }
       }
       
       // Process response data
       if let MessageBody::Response(Response::GetPeers { token, values, nodes }) = &msg.body {
           // Store token
           if let Some(candidate) = search.candidates.iter().find(|c| {
               matches!(c.status, CandidateStatus::Responded) && c.node.addr == from
           }) {
               search.nodes_with_tokens.push((candidate.node.clone(), token.clone()));
           }
           
           // Collect peers (deduplicated via HashSet)
           if let Some(peers) = values {
               for peer in peers {
                   search.peers_found.insert(peer);
               }
           }
           
           // Add closer nodes to candidates
           if let Some(new_nodes) = nodes {
               for node_info in new_nodes {
                   let already_known = search.candidates.iter().any(|c| c.node.node_id == node_info.node_id);
                   if !already_known {
                       let distance = node_info.node_id ^ search.target;
                       search.candidates.push(SearchCandidate {
                           node: node_info,
                           distance,
                           status: CandidateStatus::Pending,
                       });
                   }
               }
           }
       }
       
       // Check if search should complete
       if self.should_complete_search(search) {
           self.complete_search(&search_id).await;
       } else {
           // Continue search
           self.advance_search(search).await;
       }
   }
   ```

3. **Implement search completion logic:**
   ```rust
   fn should_complete_search(&self, search: &SearchState) -> bool {
       // C code style: 8 successful responses
       search.responded >= TARGET_RESPONSES ||
       // Or no more candidates to query and nothing in flight
       (search.in_flight == 0 && !search.candidates.iter().any(|c| matches!(c.status, CandidateStatus::Pending)))
   }
   ```

### Phase 4: Refactor iterative_get_peers

**Files to modify:**
- `src/lib.rs` (`iterative_get_peers` and related)

**Tasks:**

1. **Rewrite `iterative_get_peers`:**
   ```rust
   async fn iterative_get_peers(
       &mut self,
       info_hash: InfoHash,
       resp_tx: oneshot::Sender<Result<GetPeersResult, DhtError>>,
   ) {
       let target = NodeId::from(&info_hash);
       let initial = self.routing_table.get_closest_nodes(&target, K);
       
       if initial.is_empty() {
           tracing::debug!("iterative_get_peers: no initial candidates");
           let result = GetPeersResult {
               peers: Vec::new(),
               nodes_contacted: 0,
               nodes_with_tokens: Vec::new(),
           };
           let _ = resp_tx.send(Ok(result));
           return;
       }
       
       // Create candidates from initial nodes
       let candidates: BinaryHeap<_> = initial
           .into_iter()
           .map(|n| {
               let node = CompactNodeInfo {
                   node_id: n.node_id,
                   addr: n.addr,
               };
               SearchCandidate {
                   distance: n.node_id ^ target,
                   node,
                   status: CandidateStatus::Pending,
               }
           })
           .collect();
       
       // Create search state
       let mut search = SearchState {
           info_hash,
           target,
           candidates,
           in_flight: 0,
           responded: 0,
           peers_found: HashSet::new(),
           nodes_with_tokens: Vec::new(),
           completion_tx: Some(resp_tx),
           started_at: Instant::now(),
       };
       
       // Start initial queries (up to 3)
       self.advance_search(&mut search).await;
       
       // Store search - it will continue in background
       self.search_manager.searches.insert(info_hash, search);
   }
   ```

2. **Implement `complete_search`:**
   ```rust
   async fn complete_search(&mut self, search_id: &InfoHash) {
       if let Some(search) = self.search_manager.searches.remove(search_id) {
           // Clean up any remaining TX mappings
           for candidate in &search.candidates {
               if let CandidateStatus::Querying(tx_id) = candidate.status {
                   self.search_manager.tx_to_search.remove(&tx_id);
               }
           }
           
           // Prepare result
           let result = GetPeersResult {
               peers: search.peers_found.into_iter().collect(),
               nodes_contacted: search.responded,
               nodes_with_tokens: search.nodes_with_tokens,
           };
           
           // Send result
           if let Some(tx) = search.completion_tx {
               let _ = tx.send(Ok(result));
           }
           
           tracing::info!(
               "Search completed for {}: {} peers from {} nodes",
               search_id,
               result.peers.len(),
               result.nodes_contacted
           );
       }
   }
   ```

3. **Handle edge cases:**
   - Empty routing table: Return empty result immediately
   - All candidates fail: Complete with whatever we have
   - Early completion: If we get 8 responses quickly, complete early

### Phase 5: Update GetPeersResult

**Files to modify:**
- `src/lib.rs` (result types)

**Tasks:**

1. **Update `GetPeersResult`**:
   ```rust
   pub struct GetPeersResult {
       /// Unique peers found (deduplicated via HashSet during search)
       pub peers: Vec<SocketAddrV4>,
       /// Number of nodes that responded
       pub nodes_contacted: usize,
       /// Closest nodes with their tokens
       pub nodes_with_tokens: Vec<(CompactNodeInfo, Vec<u8>)>,
   }
   ```

2. **Ensure peers are deduplicated**: Already handled via `HashSet<SocketAddrV4>` in SearchState

## Testing Strategy

### Unit Tests

1. **SearchState construction**
   ```rust
   #[test]
   fn test_search_candidate_ordering() {
       // Verify candidates are ordered by distance
   }
   ```

2. **Candidate status transitions**
   - Pending -> Querying
   - Querying -> Responded
   - Querying -> Failed (timeout)

3. **Search completion criteria**
   - Completes at 8 responses
   - Completes when no more candidates
   - Does not complete with in-flight queries

4. **Peer deduplication**
   - Same peer from multiple nodes only appears once
   - Different peers all appear

### Integration Tests

1. **Concurrent queries**
   - Verify up to 3 queries sent simultaneously
   - Verify new queries sent as responses arrive
   - Verify timeout handling

2. **Search lifecycle**
   - Start -> responses -> complete
   - Start -> partial responses -> timeout -> complete
   - Multiple concurrent searches

3. **API compatibility**
   - Existing get_peers calls work unchanged

## API Compatibility

The public API remains unchanged:

```rust
impl Dht {
    pub async fn get_peers(&self, info_hash: InfoHash) -> Result<GetPeersResult, DhtError> {
        // Implementation changes internally, but API is identical
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(DhtCommand::GetPeers { info_hash, resp: tx })
            .await
            .map_err(|_| DhtError::ChannelClosed)?;
        rx.await.map_err(|_| DhtError::ChannelClosed)?
    }
}
```

## Performance Expectations

- **Before**: Sequential queries, ~2s per query, ~6s for typical search
- **After**: 3 concurrent queries, ~2s for 3 queries, ~4s for typical search
- **With good network**: Can complete in 2-3 round trips (4-6s) vs 6-8 round trips (12-16s)
- **Expected improvement**: 30-50% faster search completion

## Code Organization

```
src/
├── lib.rs                    # Main module
├── search.rs                 # NEW: Search state management
│   ├── SearchState
│   ├── SearchCandidate
│   ├── CandidateStatus
│   ├── SearchManager
│   └── Constants
├── lib.rs (existing)
    ├── DhtActor
    │   ├── search_manager: SearchManager           [NEW FIELD]
    │   ├── iterative_get_peers                     [MODIFIED]
    │   ├── send_query                              [NEW]
    │   ├── check_timeouts                          [NEW]
    │   ├── advance_search                          [NEW]
    │   ├── handle_response                         [NEW]
    │   ├── should_complete_search                  [NEW]
    │   ├── complete_search                         [NEW]
    │   └── run                                     [MODIFIED]
```

## Migration Guide

### For Users of the Library

No changes required. The API is identical:

```rust
let dht = Dht::new(None).await?;
dht.bootstrap().await?;
let result = dht.get_peers(info_hash).await?;
// result.peers is now deduplicated and returned faster
```

### For Developers

1. Pull the changes
2. Run tests: `cargo test`
3. Verify behavior with existing code
4. No breaking changes expected

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Race conditions in search state | Medium | High | Single-threaded actor, no shared mutable state |
| Memory leak in search manager | Low | Medium | Clean up completed searches, timeout old ones |
| Duplicate TX IDs | Low | Medium | Use AtomicU16 with wrapping |
| API breakage | Low | High | Maintain exact same public API |
| Performance regression | Low | Medium | Benchmark before/after |

## Success Criteria

- [ ] 3 concurrent queries execute simultaneously
- [ ] Search completes at 8 successful responses (or no more candidates)
- [ ] Peers are deduplicated in results
- [ ] All existing tests pass
- [ ] Search completes 30-50% faster on average
- [ ] No API breaking changes
- [ ] Code passes clippy and rustfmt

## Future Enhancements (Out of Scope)

These are noted for future PRs but NOT included here:

1. **Persistence**: Save/load node ID and routing table
2. **Rate limiting**: Token bucket for outgoing queries
3. **IPv6 support**: Dual-stack socket handling
4. **Metrics**: Track search duration, success rates
5. **Search timeouts**: Maximum search duration limit
6. **Multiple searches**: Queue when at capacity

## References

- Reference C implementation: `dht.c` lines 196-214 (constants), 1186-1266 (search_step)
- BEP 0005: DHT Protocol
- Kademlia paper: Original algorithm design

---

## Implementation Checklist

### Phase 1: Types and Constants
- [ ] Create `src/search.rs`
- [ ] Add SearchState struct
- [ ] Add SearchCandidate struct
- [ ] Add CandidateStatus enum
- [ ] Add SearchManager struct
- [ ] Add MAX_INFLIGHT_QUERIES constant (3)
- [ ] Add TARGET_RESPONSES constant (8)
- [ ] Implement Ord for SearchCandidate
- [ ] Add `pub mod search` to lib.rs

### Phase 2: Query System
- [ ] Update PendingRequest to track search_id
- [ ] Implement send_query method
- [ ] Implement check_timeouts method
- [ ] Implement advance_search method

### Phase 3: Response Handling
- [ ] Refactor handle_incoming to use handle_response
- [ ] Implement handle_response method
- [ ] Implement should_complete_search method
- [ ] Implement complete_search method

### Phase 4: Refactor
- [ ] Rewrite iterative_get_peers to use new system
- [ ] Update GetPeersResult peers field to Vec
- [ ] Update DhtActor::run with timeout checking branch
- [ ] Add search_manager field to DhtActor
- [ ] Update DhtActor::new to initialize search_manager

### Phase 5: Testing
- [ ] Add unit tests for search state
- [ ] Add integration tests for concurrent queries
- [ ] Verify API compatibility
- [ ] Benchmark performance

### Phase 6: Cleanup
- [ ] Run `cargo clippy -- -D warnings`
- [ ] Run `cargo fmt`
- [ ] Update documentation
- [ ] Create PR description with performance numbers
