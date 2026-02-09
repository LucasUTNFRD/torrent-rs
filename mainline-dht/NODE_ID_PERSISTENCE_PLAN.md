# Node ID Persistence Implementation Plan

## Overview
This document outlines the plan for implementing node ID persistence in the mainline-dht Rust implementation. Similar to the C implementation (dht-example.c), we will persist the node's ID to a file so it survives restarts.

## Goals
- Persist the 20-byte node ID to disk across application restarts
- Use the same node ID forever (unless manually deleted)
- Maintain backward compatibility - don't break existing API
- Make it optional/configurable

## Files to Modify

### 1. `Cargo.toml`
Add dependencies for file I/O and path handling:
```toml
[dependencies]
# Add these:
dirs = "5.0"          # For getting appropriate config/cache directories
```

### 2. `src/node_id.rs`
Add serialization/deserialization methods:

```rust
impl NodeId {
    /// Load node ID from a file, or generate a new one if file doesn't exist
    pub fn load_or_generate(path: &Path) -> io::Result<Self> {
        if path.exists() {
            let bytes = fs::read(path)?;
            if bytes.len() == 20 {
                let mut arr = [0u8; 20];
                arr.copy_from_slice(&bytes);
                Ok(NodeId(arr))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid node ID file: expected 20 bytes"
                ))
            }
        } else {
            // Generate new ID and save it
            let id = Self::generate_random();
            id.save(path)?;
            Ok(id)
        }
    }
    
    /// Save node ID to a file
    pub fn save(&self, path: &Path) -> io::Result<()> {
        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &self.0)?;
        Ok(())
    }
}
```

### 3. `src/dht.rs`

#### 3.1 Add configuration struct
```rust
/// Configuration for DHT node ID persistence
#[derive(Debug, Clone)]
pub struct DhtConfig {
    /// Path to store the node ID file. If None, generates random ID on each start
    pub id_file_path: Option<PathBuf>,
    /// Network port to bind to
    pub port: u16,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            id_file_path: None,  // Default: no persistence
            port: DEFAULT_PORT,
        }
    }
}
```

#### 3.2 Modify `Dht::new()` to accept configuration
```rust
impl Dht {
    /// Create a new DHT node with optional configuration
    ///
    /// # Example
    /// ```rust
    /// // With default config (random ID each time)
    /// let dht = Dht::new(None).await?;
    ///
    /// // With persistence
    /// let config = DhtConfig {
    ///     id_file_path: Some(PathBuf::from("my-node.id")),
    ///     port: 6881,
    /// };
    /// let dht = Dht::with_config(config).await?;
    /// ```
    pub async fn new(port: Option<u16>) -> Result<Self, DhtError> {
        let config = DhtConfig {
            port: port.unwrap_or(DEFAULT_PORT),
            ..Default::default()
        };
        Self::with_config(config).await
    }
    
    /// Create a new DHT node with full configuration
    pub async fn with_config(config: DhtConfig) -> Result<Self, DhtError> {
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, config.port);
        let socket = UdpSocket::bind(bind_addr).await?;
        let socket = Arc::new(socket);
        
        // Load or generate node ID
        let initial_id = if let Some(ref path) = config.id_file_path {
            match NodeId::load_or_generate(path) {
                Ok(id) => {
                    tracing::info!("Loaded node ID from {}", path.display());
                    id
                }
                Err(e) => {
                    tracing::warn!("Failed to load node ID from {}: {}, generating new", 
                        path.display(), e);
                    NodeId::generate_random()
                }
            }
        } else {
            NodeId::generate_random()
        };
        
        let node_id = Arc::new(std::sync::RwLock::new(initial_id));
        let (command_tx, command_rx) = mpsc::channel(32);
        
        let actor = DhtActor::new(socket, initial_id, command_rx);
        
        let node_id_clone = node_id.clone();
        tokio::spawn(async move {
            if let Err(e) = actor.run(node_id_clone).await {
                tracing::error!("DHT actor error: {e}");
            }
        });
        
        Ok(Dht {
            command_tx,
            node_id,
        })
    }
}
```

#### 3.3 Add helper for default persistence path
```rust
impl DhtConfig {
    /// Create a config with default persistence in the user's config directory
    /// 
    /// On Linux: ~/.config/mainline-dht/node.id
    /// On macOS: ~/Library/Application Support/mainline-dht/node.id  
    /// On Windows: %APPDATA%/mainline-dht/node.id
    pub fn with_default_persistence(port: u16) -> Result<Self, DhtError> {
        let project_dirs = directories::ProjectDirs::from("com", "your-org", "mainline-dht")
            .ok_or_else(|| DhtError::Other("Could not determine config directory".to_string()))?;
        
        let config_dir = project_dirs.config_dir();
        let id_path = config_dir.join("node.id");
        
        Ok(Self {
            id_file_path: Some(id_path),
            port,
        })
    }
}
```

### 4. `src/main.rs`
Add CLI option for ID file path:

```rust
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// info_hash to lookup peers for (hex string)
    infohash: String,
    
    /// Path to node ID file (persists identity across restarts)
    #[arg(short = 'i', long, default_value = None)]
    id_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    
    let info_hash = InfoHash::from_hex(&cli.infohash)
        .expect("Invalid info_hash: must be a 40-character hex string");
    
    println!("Creating DHT node...");
    
    let config = if let Some(id_path) = cli.id_file {
        DhtConfig {
            id_file_path: Some(id_path),
            port: 6881,
        }
    } else {
        DhtConfig::with_default_persistence(6881)?
    };
    
    let dht = Dht::with_config(config).await?;
    
    // ... rest of main
}
```

## Implementation Steps

### Step 1: Add Dependencies
Add `dirs` or `directories` crate to `Cargo.toml` for cross-platform config directory support.

### Step 2: Implement NodeId Persistence Methods
- Add `load_or_generate()` and `save()` methods to `NodeId`
- Handle file I/O errors gracefully (fallback to random generation)

### Step 3: Create DhtConfig
- Add `DhtConfig` struct with `id_file_path` and `port` fields
- Implement `Default` trait
- Add helper `with_default_persistence()` method

### Step 4: Refactor Dht::new()
- Rename current logic to `with_config()`
- Make `new()` a convenience wrapper that uses default config
- Update `new()` to load/generate ID based on config

### Step 5: Update main.rs
- Add CLI argument for ID file path
- Use `DhtConfig` instead of calling `Dht::new()` directly

### Step 6: Testing
1. **Fresh start**: No ID file exists → generates new ID and saves it
2. **Restart**: ID file exists → loads same ID
3. **Corrupted file**: Invalid ID file → generates new ID with warning
4. **No persistence**: Config without path → random ID each time (backward compatible)

## Behavior

### With Persistence (recommended):
```bash
# First run - generates ID and saves to ~/.config/mainline-dht/node.id
cargo run -- 1234567890abcdef...
# Output: Node ID: 5fbfb...

# Second run - loads same ID
cargo run -- 1234567890abcdef...
# Output: Node ID: 5fbfb... (same as before)
```

### Without Persistence (backward compatible):
```bash
# Each run gets a new random ID
cargo run -- 1234567890abcdef...
# Output: Node ID: a1b2c... (different each time)
```

### Custom ID File:
```bash
cargo run -- -i /tmp/my-node.id 1234567890abcdef...
```

## Benefits

1. **Network Stability**: Maintains routing table position across restarts
2. **Reputation**: Preserves "good node" standing in other peers' routing tables
3. **Content Responsibility**: Consistent responsibility for infohash keyspace
4. **Bootstrap Efficiency**: Can reconnect with known nodes faster
5. **Optional**: Applications can still use random IDs if they prefer

## Notes

- The ID is generated once and never updated (same as C implementation)
- To get a new ID, user must manually delete the ID file
- File permissions should be restrictive (user-only read/write)
- Consider adding a method to explicitly rotate/regenerate the ID if needed
- For BEP 42 secure IDs, the random tail bytes are persisted, and the prefix is recalculated based on external IP during bootstrap
