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

## Crates

| Crate | Description |
|-------|-------------|
| `bittorrent-cli` | Command-line interface for downloading and seeding |
| `bittorrent-daemon` | Headless daemon service (binary: `btd`) |
| `bittorrent-remote` | Remote control client for the daemon |
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

### Download

Download a torrent file:

```bash
cargo run -p bittorrent-cli -- path/to/file.torrent
```

Download to a specific directory:

```bash
cargo run -p bittorrent-cli -- path/to/file.torrent --save-dir /path/to/save
```

Download a magnet link:

```bash
cargo run -p bittorrent-cli -- "magnet:?xt=urn:btih:..."
```

### Seed

Seed existing content:

```bash
cargo run -p bittorrent-cli -- path/to/file.torrent --watch-dir /path/to/content
```

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `--port, -p` | Listening port for incoming connections | 6881 |
| `--save-dir, -d` | Directory to save downloaded files | `$HOME/Downloads/Torrents` |
| `--watch-dir, -w` | Directory containing files to seed | - |
| `--log-level` | Log level (error, warn, info, debug, trace) | info |

## License

MIT
