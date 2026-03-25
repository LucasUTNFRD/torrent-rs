# torrent-rs

A BitTorrent client in Rust.

## Binaries

| Binary | Crate | Description |
|--------|-------|-------------|
| `torrent-rs` | `cmd` | CLI downloader |
| `tui` | `tui` | Interactive terminal UI |

### CLI

```bash
# Download a torrent
cargo run -p cmd -- download <file.torrent|magnet:?>

# Add without starting (list/stats are stubs)
cargo run -p cmd -- add <file.torrent|magnet:?>
cargo run -p cmd -- list
cargo run -p cmd -- stats
```

Options:
- `--metrics-addr <ADDR>` — Prometheus endpoint (default: `0.0.0.0:9000`)

### TUI

```bash
cargo run -p tui
```

Keybindings:
- `a` — add torrent (path or magnet URI)
- `d` — remove selected torrent
- `Enter` — open detail view
- `Tab` / `Shift+Tab` — switch detail tabs
- `Esc` / `q` — close detail view
- `q` — quit

## BEPs

| BEP | Description |
|-----|-------------|
| 3 | Core protocol |
| 5 | DHT peer discovery |
| 9 | Magnet URI |
| 10 | Peer extensions |
| 15 | UDP tracker |
| 23 | Compact peer format |

## Crates

| Crate | Purpose |
|-------|---------|
| `bittorrent-core` | Session management, peer connections, piece coordination |
| `bittorrent-common` | Shared types: `InfoHash`, `PeerId`, hashing |
| `bencode` | Bencode codec |
| `magnet-uri` | Magnet link parser |
| `mainline-dht` | DHT implementation |
| `tracker-client` | HTTP and UDP tracker client |
| `cmd` | CLI binary |
| `tui` | Terminal UI |

## Build

```bash
just build
```

Or directly: `cargo build --release`

Requires Rust 2024 edition (Rust 1.85+) and optionally [just](https://github.com/casey/just).

## Development

```bash
just test            # unit tests
just test-simulation # network simulation tests (turmoil)
just lint            # clippy
```

## Observability

```bash
just metrics-up    # start Prometheus + Grafana
just metrics-down  # stop
```

The CLI exposes metrics at the configured `--metrics-addr`.

## License

MIT