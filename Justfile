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

# Download a torrent file
download torrent:
    cargo run -p bittorrent-cli -- "{{torrent}}"

# Download a torrent to a specific directory
download-to torrent save_dir:
    cargo run -p bittorrent-cli -- "{{torrent}}" --save-dir "{{save_dir}}"

# Download a torrent with a custom port
download-port torrent port:
    cargo run -p bittorrent-cli -- "{{torrent}}" --port {{port}}

# Download with debug logging
download-debug torrent:
    cargo run -p bittorrent-cli -- "{{torrent}}" --log-level debug

# Seed existing content from a directory
seed torrent content_dir:
    cargo run -p bittorrent-cli -- "{{torrent}}" --watch-dir "{{content_dir}}"

# Download a magnet link
magnet uri:
    cargo run -p bittorrent-cli -- "{{uri}}"

# Download a magnet link with debug logging
magnet-debug uri:
    cargo run -p bittorrent-cli -- "{{uri}}" --log-level debug

# Run the BitTorrent Daemon
daemon *args:
    cargo run -p bittorrent-daemon -- {{args}}

# Run the BitTorrent Remote client
remote *args:
    cargo run -p bittorrent-remote -- {{args}}
