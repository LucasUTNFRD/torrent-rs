# PR: Event-loop based DHT with streaming get_peers + Choke Manager

## Title

```
refactor: Event-loop based DHT with streaming get_peers + Choke Manager
```

## Description

```markdown
## Summary
This PR introduces a major architectural refactoring of the DHT implementation and adds the choke manager for better upload slot management.

## Key Changes

### DHT Refactoring
- **Event-loop based architecture**: Replaced await-based search with single-threaded transaction manager
- **Streaming peer discovery**: `get_peers()` now returns `mpsc::Receiver` for real-time peer streaming
- **Transaction management**: New `transaction.rs` module for tracking in-flight queries with timeout/retry logic
- **IPv6 support**: Changed `SocketAddrV4` to `SocketAddr` throughout

### Choke Manager
- Implemented BitTorrent choking algorithm with configurable upload slots
- Round-robin slot assignment for fairness
- Proper cleanup on peer disconnection

### Bug Fixes
- Fixed bitfield index out of bounds
- Reset pieces on hash mismatch
- Improved peer metrics tracking

## Breaking Changes
- `DhtHandler::get_peers()` signature changed: now returns `mpsc::Receiver` instead of `Result`
- `bootstrap()` no longer returns `NodeId`

## Testing
- [ ] DHT bootstrap and peer discovery
- [ ] Choke manager with multiple peers
- [ ] IPv6 connectivity (if available)

## Files Changed
24 files, +2,646/-1,532 lines
```

## Code Review Notes

### Highlights
- Well-structured refactoring with clear separation of concerns
- Good documentation in new modules (`choker.rs`, `transaction.rs`)
- Streaming API for `get_peers()` is a significant improvement for real-time peer discovery
- Proper error handling patterns throughout

### Suggestions
- Consider filling in `mainline-dht/MECHANISM.md` with architecture documentation
- Add unit tests for `TransactionManager` edge cases (timeouts, retries)
- Consider adding metrics/logging for choker decisions

### Commits Included
```
ce5894f refactor: complete async dht operation + stream based get_peers api refactoring
7f8771e refactor: get peers now uses stream based mechanism for getting peers
9ccff64 refactor to event-loop based
86b1225 add choker
3f3cb24 implement periodic chokes
... (35 total commits)
```
