# DHT Implementation Issues (mainline-dht)

## Remaining Issues to Fix

### 1. Bootstrap Not Properly Iterative (High Priority)

**Current**: `do_bootstrap()` sends find_node to closest nodes periodically but doesn't track if closer nodes are found.

**BEP 05 Requires**:
> Upon inserting the first node into its routing table and when starting up thereafter, the node should attempt to find the closest nodes in the DHT to itself. It does this by issuing find_node messages to closer and closer nodes until it cannot find any closer.

**What's needed**:
- Track `closest_nodes` (sorted by distance to local_id)
- Track `queried_nodes` (nodes already queried)
- Query next α closest unqueried nodes
- Continue until no closer nodes found for α iterations

### 2. Get Peers Not Properly Iterative (High Priority)

**Current**: `start_get_peers()` fires initial queries and relies on response handlers to continue, but:
- No convergence tracking
- No termination condition
- May not find closest nodes

**What's needed**:
```rust
struct IterativeLookup {
    target: NodeId,
    closest: BinaryHeap<Node>,// by distance
    queried: HashSet<SocketAddr>,
    pending: HashSet<SocketAddr>,
    alpha: usize, // concurrency (typically 3)
}

// Terminate when:
// - All α closest nodes have been queried
// - No pending queries remain
// - No closer nodes found in last round
```

### 3. Missing Bucket Refresh (Medium Priority)

**BEP 05**:
> Buckets that have not been changed in 15 minutes should be "refreshed."

**Current**: `perform_maintenance()` does send find_node for stale buckets, but:
- Doesn't verify bucket actually needs refresh
- Should track refresh success

## What Was Fixed

1. ✅ **Node Status Tracking** (`node.rs`):
   - Added `last_response`, `last_query_from`, `failed_queries`
   - Implemented proper `is_good()` per BEP 05 definition
   - Added `mark_response()`, `mark_query_from()`, `mark_failed()`

2. ✅ **Bucket Replacement Policy** (`routing_table.rs`):
   - `try_add_node()` now returns questionable nodes to ping
   - When bucket is full: check for questionable nodes and ping LRU
   - Follows BEP 05 bucket split/replacement rules

3. ✅ **Incoming Query Updates Node Status** (`dht.rs`):
   - `handle_query()` now calls `mark_node_query_from()`
   - Keeps nodes "good" when they send us queries

## Notes

- The `mark_failed()` method is implemented but not yet used in query timeout handling
- Need to integrate `mark_failed()` when queries timeout after retries
- Consider adding a proper iterative lookup state machine for both bootstrap and get_peers