// ---------------------------------------------------------------------------
// DhtConfig — validated value type
// ---------------------------------------------------------------------------

use std::path::PathBuf;

use crate::error::DhtError;

/// Default port for DHT.
const DEFAULT_PORT: u16 = 6881;

#[derive(Debug, Clone)]
pub struct DhtConfig {
    pub id_file_path: Option<PathBuf>,
    pub state_file_path: Option<PathBuf>,
    pub port: u16,
    /// Bootstrap nodes used when the routing table is empty.
    /// Defaults to [`default_dht_bootstrap_nodes()`] if not set.
    pub bootstrap_nodes: Vec<BootstrapNode>,
}

impl DhtConfig {
    pub fn builder() -> DhtConfigBuilder {
        DhtConfigBuilder::default()
    }
}

impl Default for DhtConfig {
    fn default() -> Self {
        DhtConfigBuilder::default().build()
    }
}

/// A DHT bootstrap node (host + port, unresolved).
/// Kept as strings because DNS resolution happens at connection time.
#[derive(Debug, Clone)]
pub struct BootstrapNode {
    pub host: String,
    pub port: u16,
}

impl BootstrapNode {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

/// Well-known public bootstrap nodes shipped as a convenience default.
pub fn default_dht_bootstrap_nodes() -> Vec<BootstrapNode> {
    vec![
        BootstrapNode::new("router.bittorrent.com", 6881),
        BootstrapNode::new("dht.transmissionbt.com", 6881),
        BootstrapNode::new("dht.libtorrent.org", 25401),
        BootstrapNode::new("router.utorrent.com", 6881),
    ]
}

// ---------------------------------------------------------------------------
// DhtConfigBuilder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DhtConfigBuilder {
    id_file_path: Option<PathBuf>,
    state_file_path: Option<PathBuf>,
    port: u16,
    bootstrap_nodes: Option<Vec<BootstrapNode>>,
}

impl Default for DhtConfigBuilder {
    fn default() -> Self {
        Self {
            id_file_path: None,
            state_file_path: None,
            port: DEFAULT_PORT,
            bootstrap_nodes: None,
        }
    }
}

impl DhtConfigBuilder {
    pub fn id_file_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.id_file_path = Some(path.into());
        self
    }

    pub fn state_file_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_file_path = Some(path.into());
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Replace the bootstrap list entirely.
    pub fn bootstrap_nodes(mut self, nodes: Vec<BootstrapNode>) -> Self {
        self.bootstrap_nodes = Some(nodes);
        self
    }

    /// Append a single node to the list (initialises from defaults if not yet set).
    pub fn bootstrap_node(mut self, host: impl Into<String>, port: u16) -> Self {
        self.bootstrap_nodes
            .get_or_insert_with(default_dht_bootstrap_nodes)
            .push(BootstrapNode::new(host, port));
        self
    }

    /// Enable default persistence in the OS config directory.
    pub fn with_default_persistence(mut self) -> Result<Self, DhtError> {
        let dirs = directories::ProjectDirs::from("com", "mainline", "mainline-dht")
            .ok_or_else(|| DhtError::Other("Could not determine config directory".to_string()))?;
        let config_dir = dirs.config_dir().to_path_buf();
        self.id_file_path = Some(config_dir.join("node.id"));
        self.state_file_path = Some(config_dir.join("dht_state.dat"));
        Ok(self)
    }

    pub fn build(self) -> DhtConfig {
        DhtConfig {
            id_file_path: self.id_file_path,
            state_file_path: self.state_file_path,
            port: self.port,
            bootstrap_nodes: self
                .bootstrap_nodes
                .unwrap_or_else(default_dht_bootstrap_nodes),
        }
    }
}
