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

# Run the BitTorrent CLI
cli *args:
    cargo run -p bittorrent-cli -- {{args}}

# Add a torrent via CLI
add torrent:
    cargo run -p bittorrent-cli -- add "{{torrent}}"

# Add and follow a torrent via CLI
add-follow torrent:
    cargo run -p bittorrent-cli -- add "{{torrent}}" --follow

# List torrents via CLI
list:
    cargo run -p bittorrent-cli -- list

# Show session stats via CLI
stats:
    cargo run -p bittorrent-cli -- stats

# Run the BitTorrent TUI
tui *args:
    cargo run -p bittorrent-tui -- {{args}}

# Run the BitTorrent Daemon
daemon *args:
    cargo run -p bittorrent-daemon -- {{args}}

# Run the BitTorrent Remote client
remote *args:
    cargo run -p bittorrent-remote -- {{args}}
