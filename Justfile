# Default command: list all available commands
default:
    @just --list

# Build the project in release mode
build:
    cargo build --release

# Run clippy for linting
lint:
    cargo clippy --all-targets --workspace -- -D warnings

# Run clippy pedantic lints
lint-pedantic:
    cargo clippy -- -D clippy::pedantic -D clippy::nursery

# Run all workspace tests
test:
    cargo test --workspace

# Clean build artifacts
clean:
    cargo clean

test-simulation:
	RUST_LOG=info,turmoil=trace,bittorrent_core=debug cargo test -p bittorrent-core --features=sim --  --nocapture

# Metrics commands
metrics-up:
    cd metrics && docker-compose up -d

metrics-down:
    cd metrics && docker-compose down
