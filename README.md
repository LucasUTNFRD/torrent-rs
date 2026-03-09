# torrent-rs

A BitTorrent client written in Rust, built as a learning project to explore network programming and async Rust.

## Scope

This project implements a functional BitTorrent client with support for downloading and seeding torrents. It is designed for educational purposes and personal use.

## Supported BEPs

| BEP | Description | Link |
|-----|-------------|------|
| BEP 3 | Core Protocol | [bep_0003](https://www.bittorrent.org/beps/bep_0003.html) |
| BEP 5 | DHT | [bep_0005](https://www.bittorrent.org/beps/bep_0005.html) |
| BEP 9 | Magnet URI | [bep_0009](https://www.bittorrent.org/beps/bep_0009.html) |
| BEP 10 | Peer Extension | [bep_0010](https://www.bittorrent.org/beps/bep_0010.html) |
| BEP 15 | UDP Tracker | [bep_0015](https://www.bittorrent.org/beps/bep_0015.html) |
| BEP 23 | Tracker Return Compact | [bep_0023](https://www.bittorrent.org/beps/bep_0023.html) |

## Architecture

The project is split into a headless engine (**daemon**) and multiple controllers (**CLI**, **TUI**).

1. **`bittorrent-daemon` (btd)**: The main engine that manages torrent sessions, downloads, and seeding. It exposes a RESTful HTTP API.
2. **`bittorrent-tui`**: A terminal user interface to monitor and manage torrents in real-time.
3. **`bittorrent-cli`**: A command-line controller to add, list, or remove torrents via the daemon.

## Crates

| Crate | Description |
|-------|-------------|
| `bittorrent-daemon` | Headless engine (binary: `btd`) |
| `bittorrent-tui` | Terminal user interface |
| `bittorrent-cli` | Command-line controller |
| `bittorrent-remote` | Simple IPC-based remote client |
| `bittorrent-core` | Core session management and coordination |
| `bittorrent-common` | Shared types, metainfo parsing, utilities |
| `bencode` | Bencode encoding/decoding library |
| `peer-protocol` | BitTorrent wire protocol implementation |
| `tracker-client` | HTTP and UDP tracker client |
| `mainline-dht` | DHT protocol implementation (BEP 5) |
| `magnet-uri` | Magnet URI parsing |

## Prerequisites

- [Rust and Cargo](https://rustup.rs/)
- [just](https://github.com/casey/just) (optional, for using the justfile)

## Build

```bash
cargo build --release
```

## Usage

### 1. Start the Daemon

The daemon must be running to manage torrents. It listens for peer connections on port 6881 and provides an API on port 6969 by default.

```bash
cargo run -p bittorrent-daemon -- [OPTIONS] [TORRENTS...]
```

**Options:**
- `-d, --save-dir <PATH>`: Directory to save downloaded files (default: `~/Downloads/Torrents`).
- `--api-port <PORT>`: Port for the HTTP API (default: `6969`).
- `--port <PORT>`: Port for peer connections (default: `6881`).
- `--no-dht`: Disable DHT support.

### 2. Monitor with TUI

Start the TUI to see live progress of all active torrents.

```bash
cargo run -p bittorrent-tui
```

### 3. Control with CLI

Use the CLI to add, list, or manage torrents.

#### Add a torrent
```bash
cargo run -p bittorrent-cli -- add <FILE_OR_MAGNET> [--follow]
```

#### List active torrents
```bash
cargo run -p bittorrent-cli -- list
```

#### Show detailed info
```bash
cargo run -p bittorrent-cli -- show <INFO_HASH> [--follow]
```

#### Remove a torrent
```bash
cargo run -p bittorrent-cli -- remove <INFO_HASH>
```

#### Global Stats
```bash
cargo run -p bittorrent-cli -- stats
```

## License

MIT
