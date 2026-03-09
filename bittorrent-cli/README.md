# BitTorrent CLI Controller

A command-line tool to control the `bittorrent-daemon` via its HTTP API. This tool allows you to manage torrents locally or remotely.

## Usage

```bash
cargo run -p bittorrent-cli -- [COMMAND]
```

### Commands

- `add <PATH_OR_URI>`: Add a `.torrent` file or Magnet link. Use `--follow` to watch live progress.
- `list`: Show a summary of all active torrents in the daemon.
- `show <ID>`: Show detailed information (peers, files, trackers) for a torrent.
- `remove <ID>`: Stop and remove a torrent from the daemon.
- `stats`: Show aggregate daemon statistics.

### Examples

```bash
# Add a magnet link and follow progress
bittorrent-cli add "magnet:?xt=urn:btih:..." --follow

# List all torrents
bittorrent-cli list

# Show details for a specific torrent
bittorrent-cli show a1b2c3d4...
```

### Remote Control

By default, the CLI connects to `http://localhost:6969`. To control a daemon running on a different machine:

```bash
bittorrent-cli --daemon-url http://192.168.1.50:6969 list
```
