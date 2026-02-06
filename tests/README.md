# Integration Test Suite

This directory contains integration tests for the BitTorrent client, structured based on patterns from the libtransmission test suite.

## Structure

```
tests/
├── PLAN.md                   # Detailed implementation roadmap
├── README.md                 # This file
├── Cargo.toml               # Test crate configuration
├── common/                   # Shared test utilities
│   ├── mod.rs               # Module exports
│   ├── fixtures.rs          # Test fixtures (SessionTest, etc.)
│   ├── helpers.rs           # Async helpers and macros
│   ├── mocks.rs             # Mock implementations
│   └── sandbox.rs           # Temp directory management
├── assets/                   # Test data files
│   ├── torrents/            # Sample .torrent files
│   ├── dht/                 # DHT test vectors
│   └── bencode/             # Bencode test data
└── *_tests.rs               # Component test files
```

## Running Tests

```bash
# Run all integration tests
cargo test --package integration-tests

# Run specific test file
cargo test --package integration-tests --test bencode_tests
cargo test --package integration-tests --test bitfield_tests

# Run with output
cargo test --package integration-tests -- --nocapture

# Run with tracing
cargo test --package integration-tests -- --nocapture 2>&1 | bunyan
```

## Test Categories

### 1. Unit Tests
Located in `src/` files with `#[cfg(test)]`:
- Fast, isolated, no I/O
- Example: Bitfield operations, bencode parsing

### 2. Integration Tests
Located in `tests/*_tests.rs`:
- Test component interactions
- Use mocked external dependencies
- Examples:
  - `bencode_tests.rs` - Parser round-trips
  - `bitfield_tests.rs` - Bitwise operations
  - `dht_tests.rs` - DHT bootstrap and lookups
  - `tracker_tests.rs` - HTTP/UDP tracker communication

### 3. End-to-End Tests
Located in `end_to_end_tests.rs`:
- Full scenarios with real networking
- Two-client piece exchange
- Magnet link downloads

## Fixtures

### SandboxedTest
Provides a temporary directory that's automatically cleaned up:

```rust
use common::sandbox::SandboxedTest;

let sandbox = SandboxedTest::new();
let file = sandbox.create_file("test.txt", b"Hello");
// directory is cleaned up when sandbox is dropped
```

### SessionTest
Creates a full Session with sandboxed storage:

```rust
use common::fixtures::SessionTest;

#[tokio::test]
async fn test_something() {
    let fixture = SessionTest::new().await.unwrap();
    // Use fixture.session...
    fixture.shutdown().await.unwrap();
}
```

## Mocks

### MockDht
Mock DHT implementation that tracks all method calls:

```rust
use common::mocks::MockDht;

let mock = MockDht::new();
mock.set_healthy_swarm();
mock.record_search(info_hash, 6881);

assert_eq!(mock.get_searches().len(), 1);
```

### DhtState
Represents a DHT state file (dht.dat):

```rust
use common::mocks::DhtState;

let state = DhtState::new()
    .with_nodes(bootstrap_nodes);
state.save(sandbox.join("dht.dat")).unwrap();
```

## Helpers

### wait_for!
Polls a condition until true or timeout:

```rust
use common::wait_for;

let result = wait_for!(counter.load(Ordering::Relaxed) > 5, 1000);
assert!(result, "Timeout waiting for condition");
```

### wait_for_async
Async version for use in async tests:

```rust
use common::helpers::wait_for_async;

let result = wait_for_async(
    || counter.load(Ordering::Relaxed) > 5,
    1000
).await;
```

## Adding New Tests

1. Create test file: `tests/my_component_tests.rs`
2. Add to `tests/Cargo.toml` under `[[test]]` section
3. Import common utilities: `use common::{fixtures::*, mocks::*, helpers::*};`
4. Write tests using the fixtures and mocks
5. Run tests to verify: `cargo test --test my_component_tests`

## Patterns from libtransmission

1. **Hierarchical Fixtures**: SandboxedTest → SessionTest
2. **Sandboxing**: Every test gets an isolated temp directory
3. **Mock External Dependencies**: Network calls mocked for reliability
4. **Async Polling**: Use `wait_for!` instead of fixed sleeps
5. **Test Assets**: Version-controlled test data in `assets/`

## Dependencies

Test-only dependencies are declared in workspace `Cargo.toml`:

- `tokio-test` - Async test utilities
- `tempfile` - Temp directory management
- `wiremock` - HTTP mocking
- `mockall` - Trait mocking
- `serial_test` - Prevent parallel test execution when needed

## Future Improvements

- [ ] Add property-based testing with `proptest`
- [ ] Add snapshot testing with `insta`
- [ ] Create test data generators for torrents
- [ ] Add integration with testcontainers for real DHT nodes
- [ ] Add code coverage reporting
