# SESSION.md

## Bugs found during DHT persistence work

- **dht.rs L651-657**: `add_peer` is called twice for the same peer in the `AnnouncePeer` handler — likely a copy-paste bug.
- **dht.rs**: `run_periodic_ping()` and `refresh_table()` have empty bodies — routing table maintenance is not implemented.
- **dht.rs**: `search_task.rs` is an empty file not declared as a module — dead file.
