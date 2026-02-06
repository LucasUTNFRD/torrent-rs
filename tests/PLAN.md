# Integration Test Suite Plan

Based on libtransmission's test patterns adapted for Rust.

## Directory Structure

```
torrent-rs/
├── tests/                          # Integration tests at workspace root
│   ├── README.md                   # This file
│   ├── common/                     # Shared test utilities
│   │   ├── mod.rs
│   │   ├── fixtures.rs             # Test fixtures (SessionTest equivalent)
│   │   ├── mocks.rs                # Mock implementations
│   │   ├── sandbox.rs              # Temp directory management
│   │   └── helpers.rs              # wait_for!() macros, etc.
│   ├── assets/                     # Test data files
│   │   ├── torrents/               # Sample .torrent files
│   │   ├── dht/                    # DHT test vectors
│   │   └── bencode/                # Bencode test data
│   ├── bencode_tests.rs            # Bencode parsing tests
│   ├── bitfield_tests.rs           # Bitfield tests
│   ├── dht_tests.rs                # DHT integration tests
│   ├── tracker_tests.rs            # Tracker client tests
│   ├── peer_tests.rs               # Peer protocol tests
│   ├── storage_tests.rs            # Storage/disk I/O tests
│   ├── torrent_tests.rs            # Torrent lifecycle tests
│   ├── session_tests.rs            # Session management tests
│   ├── magnet_tests.rs             # Magnet link tests
│   └── end_to_end_tests.rs         # Full download scenarios
```

## Test Categories

### 1. Unit Tests (per-crate, inline)
- Location: `src/*.rs` with `#[cfg(test)]`
- Fast, isolated, no I/O
- Examples: Bitfield operations, bencode parsing, DHT message encoding

### 2. Integration Tests (tests/ directory)
- Location: `tests/*_tests.rs`
- Test component interactions with mocked external dependencies
- Examples: DHT bootstrap, tracker announce, peer handshake

### 3. End-to-End Tests (tests/ directory)
- Location: `tests/end_to_end_tests.rs`
- Full scenarios with real networking
- Examples: Two clients exchanging pieces, magnet downloads

## Key Components to Implement

### 1. Test Fixtures

Like libtransmission's hierarchy: `TransmissionTest` → `SandboxedTest` → `SessionTest`

```rust
// SandboxedTest - provides temp directory
pub struct SandboxedTest {
    temp_dir: TempDir,
}

// SessionTest - creates Session with sandboxed config  
pub struct SessionTest {
    sandbox: SandboxedTest,
    session: Session,
}
```

### 2. Mock Implementations

Like libtransmission's MockDht, MockMediator:

```rust
// MockDht - tracks calls, simulates responses
pub struct MockDht {
    good_nodes: AtomicUsize,
    pinged_nodes: Mutex<Vec<SocketAddr>>,
    searches: Mutex<Vec<InfoHash>>,
}

// MockTracker - uses wiremock
pub struct MockTracker {
    server: wiremock::MockServer,
}
```

### 3. Async Helpers

Like libtransmission's `waitFor()`:

```rust
#[macro_export]
macro_rules! wait_for {
    ($condition:expr, $timeout_ms:expr) => {{ /* ... */ }}
}
```

## Dependencies

Add to workspace Cargo.toml:
- `tokio-test = "0.4"` - Async test utilities
- `tempfile = "3"` - Temp directory management
- `wiremock = "0.6"` - HTTP mocking (already in tracker-client)
- `mockall = "0.12"` - Trait mocking
- `serial_test = "3"` - Prevent parallel test execution when needed
- `insta = "1"` - Snapshot testing for parser output

## Implementation Phases

### Phase 1: Infrastructure (Priority: High)
1. Create tests/ directory structure
2. Add dev-dependencies to workspace
3. Implement sandbox.rs - temp directory management
4. Implement helpers.rs - wait_for! macro

### Phase 2: Unit Test Review (Priority: High)
1. Review existing `#[cfg(test)]` blocks
2. Ensure unit tests are truly unit tests
3. Move integration-like tests to tests/ directory

### Phase 3: Component Tests (Priority: Medium)
1. **bencode_tests.rs** - Parsing round-trips, error cases
2. **bitfield_tests.rs** - Set/get, serialization, edge cases
3. **dht_tests.rs** - Bootstrap, node lookup, peer discovery
4. **tracker_tests.rs** - HTTP/UDP announce, scrape

### Phase 4: Integration Tests (Priority: Medium)
1. **session_tests.rs** - Session lifecycle, settings
2. **torrent_tests.rs** - Add/remove/verify torrents
3. **peer_tests.rs** - Handshake, piece exchange
4. **storage_tests.rs** - Read/write pieces

### Phase 5: End-to-End Tests (Priority: Low)
1. **magnet_tests.rs** - Metadata download via DHT
2. **end_to_end_tests.rs** - Two-client piece exchange

## Test Naming Conventions

Follow Rust conventions:
- Unit tests: `test_<function>_<scenario>`, e.g., `test_bitfield_set_get`
- Integration tests: `<component>_<scenario>_<expected>`, e.g., `dht_bootstraps_from_state_file`

## Running Tests

```bash
# All tests
cargo test

# Only integration tests
cargo test --test '*_tests'

# Specific component
cargo test dht_tests

# With real network (optional feature)
cargo test --features real_network_tests

# With output
cargo test -- --nocapture
```

## Assets

Create test data in tests/assets/:
- Sample .torrent files with known info hashes
- Bencode test vectors (valid/invalid)
- DHT message samples

## Lessons from libtransmission

1. **Every test gets a sandbox** - isolated temp directories
2. **Mock external deps** - network calls, time, randomness
3. **Async polling helpers** - don't use fixed sleeps
4. **Hierarchical fixtures** - compose test setups
5. **State file testing** - load/save state files
6. **Test assets** - version control test data
