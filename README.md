# Rust BitTorrent Client

A BitTorrent client implementation written in Rust. This project was started primarily as a learning exercise to explore network programming, asynchronous Rust, and idiomatic Rust practices while building something functional.

**[Current Status - Experimental]**
This client is currently under active development. While basic download functionality might work, expect bugs and incomplete features. It is primarily intended for educational purposes at this stage.

## Features

* Parses `.torrent` metadata files.
* Decodes/Encodes Bencode data format.
* Communicates with HTTP/UDP trackers to announce and fetch peers.
* Connects to peers using the standard BitTorrent peer protocol.
* Implements peer wire messages.
* Downloads either a single file or a mutli file torrent from peers.
* Async I/O done with `tokio`.


## Getting Started

### Prerequisites

Ensure you have the following installed:

- **Rust**: Install Rust using [rustup](https://rustup.rs/).
- **Cargo**: Comes bundled with Rust.

### Running the Client

1. Clone the repository:

   ```bash
   git https://github.com/LucasUTNFRD/torrent-rs.git rust_torrent
   cd rust_torrent
   ```

2. Build the project:
   ```bash
   make build
   ```

3. Run the client:

   ```bash
   cargo run -- path/to/torrent/file.torrent
   ```

  Replace `path/to/torrent/file.torrent` with the path to a `.torrent` file you want to download.

### CLI Options

The following options are available for the CLI:

- **`torrent_file`** (required): Path to the `.torrent` file to download.
- **`--port`, `-p`** (optional): Listening port for incoming peer connections. Default: `6881`.
- **`--save-dir`, `-d`** (optional): Directory to save downloaded files. Defaults to `$HOME/Downloads/Torrents/`.
- **`--log-level`** (optional): Set the log level. Options:
  - `error`
  - `warn`
  - `info` (default)
  - `debug`
  - `trace`

### Example Usage

1. Download a torrent with default settings:
   ```bash
   cargo run -- path/to/torrent/file.torrent
   ```

2. Specify a custom save directory:
   ```bash
   cargo run -- path/to/torrent/file.torrent --save-dir /path/to/save
   ```

3. Change the listening port and log level:
   ```bash
   cargo run -- path/to/torrent/file.torrent --port 7000 --log-level debug
   ```

4. Use verbose logging for debugging:
   ```bash
   cargo run -- path/to/torrent/file.torrent --log-level trace
   ```


## Roadmap

| Nº  | Milestone                              | Status  | BEP                                                      |
| --- | -------------------------------------- | ------- | -------------------------------------------------------- |
| 1   | Core Protocol Implementation           | 🏗️ WIP | [BEP 0003](http://www.bittorrent.org/beps/bep_0003.html) |
| 2   | User Interface (TUI/CLI)               | ✅       | N/A                                                      |
| 3   | Comprehensive Testing & Benchmarking   | ❌ To Do | N/A                                                      |
| 4   | UDP Tracker Protocol                   | ✅       | [BEP 0015](http://www.bittorrent.org/beps/bep_0015.html) |
| 5   | DHT Protocol                           | ❌ To Do | [BEP 0005](https://bittorrent.org/beps/bep_0005.html)    |
| 6   | Magnet URI Scheme                      | ❌ To Do | [BEP 0009](http://www.bittorrent.org/beps/bep_0009.html) |
| 7   | Fast Extension                         | ❌ To Do | [BEP 0006](http://www.bittorrent.org/beps/bep_0006.html) |
| 8   | Performance Optimizations & Refinement | ❌ To Do | N/A                                                      |
---

*(Status Icons: ✅ Done, 🏗️ WIP / In Progress, ❌ To Do)*

## Known Limitations

This client lacks several critical features required for modern torrent functionality. As a result, it will fail to download files under most common conditions:

* No Magnet Link Support: It cannot handle magnet URIs as it lacks the DHT protocol needed to find peers without a tracker.

* No DHT Protocol: It cannot participate in the Distributed Hash Table (DHT) network, which is the primary method for decentralized peer discovery.

* No Web Seed Support: It cannot download data from HTTP/FTP web servers acting as seeds.
