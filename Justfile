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

run-example  MAGNET:
    cargo run --example download_with_progress -- "{{MAGNET}}"

download-debug level="debug":
    rm -rf $HOME/Downloads/Torrents/Big\ Buck\ Bunny/
    RUST_LOG="{{level}}" cargo run  --example download_with_progress -- "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Fexplodie.org%3A6969&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969&tr=udp%3A%2F%2Ftracker.empire-js.us%3A1337&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=wss%3A%2F%2Ftracker.btorrent.xyz&tr=wss%3A%2F%2Ftracker.fastcast.nz&tr=wss%3A%2F%2Ftracker.openwebtorrent.com&ws=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2F&xs=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2Fbig-buck-bunny.torrent"

test-simulation:
	RUST_LOG=info,turmoil=trace,bittorrent_core=debug cargo test -p bittorrent-core --features=sim --  --nocapture
