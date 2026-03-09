# BitTorrent Daemon (`btd`)

A headless BitTorrent engine that runs as a background service. It manages multiple torrent sessions and exposes a RESTful HTTP API for remote control.

## Running

```bash
cargo run -p bittorrent-daemon -- [ARGS]
```

### Options
- `--port`: Listening port for BitTorrent peer connections (default: `6881`).
- `--api-port`: Port for the HTTP API (default: `6969`).
- `--save-dir`: Directory where files will be downloaded.
- `--no-dht`: Disable Distributed Hash Table support.

---

## HTTP API

The daemon provides a JSON API for integration with dashboards, TUIs, and scripts.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/torrents` | List all torrents (summary) |
| `POST` | `/torrents` | Add a new torrent/magnet |
| `GET` | `/torrents/:id` | Get detailed stats for a specific torrent |
| `DELETE` | `/torrents/:id` | Remove a torrent |
| `GET` | `/stats` | Global session statistics |

### Data Models

#### Add Torrent (`POST /torrents`)
**Request Body:**
```json
{
  "path": "/absolute/path/to/file.torrent",
  "uri": "magnet:?xt=urn:btih:..."
}
```
*Note: Provide either `path` or `uri`.*

#### Torrent Details (`GET /torrents/:id`)
**Response Body:**
```json
{
  "id": "info_hash_hex",
  "name": "Example Torrent",
  "state": "Downloading",
  "progress": 0.45,
  "download_rate": 102400,
  "upload_rate": 5120,
  "peers_connected": 12,
  "peers_discovered": 45,
  "size_bytes": 1073741824,
  "downloaded_bytes": 483183820,
  "peers": [
    { "id": "...", "client_id": "...", "ip": "1.2.3.4", "rate_up": 800, "rate_down": 12000 }
  ],
  "trackers": [
    { "url": "http://tracker.com/announce", "error": null, "last_report": "2023-10-27T10:00:00Z" }
  ],
  "files": [
    { "path": "movie.mp4", "size": 1073741824, "progress": 0.45 }
  ]
}
```
