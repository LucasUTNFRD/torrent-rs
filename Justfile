# Default command: list all available commands
default:
    @just --list

# Build the project in release mode
build:
    cargo build --release

# Run clippy for linting
lint:
    cargo clippy --all-targets --workspace -- -D warnings

# Run all workspace tests
test:
    cargo test --workspace

# Clean build artifacts
clean:
    cargo clean

# Run the BitTorrent CLI
cli *args:
    cargo run -p bittorrent-cli -- {{args}}

# Run the BitTorrent Daemon
daemon *args:
    cargo run -p bittorrent-daemon -- {{args}}

# Run the BitTorrent Remote client
remote *args:
    cargo run -p bittorrent-remote -- {{args}}
