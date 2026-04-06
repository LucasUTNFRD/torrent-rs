use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::NonZeroU32,
    path::PathBuf,
    time::Duration,
};

use bittorrent_common::types::PeerID;
use directories::ProjectDirs;
use mainline_dht::{BootstrapNode, default_dht_bootstrap_nodes};

// ---------------------------------------------------------------------------
// Sub-types
// ---------------------------------------------------------------------------

/// A resolved listen interface. Holds addr + port separately so the port can
/// be queried without constructing a SocketAddr first.
#[derive(Debug, Clone)]
pub struct ListenInterface {
    pub addr: IpAddr,
    pub port: u16,
}

impl ListenInterface {
    pub fn new(addr: impl Into<IpAddr>, port: u16) -> Self {
        Self {
            addr: addr.into(),
            port,
        }
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.addr, self.port)
    }
}

impl Default for ListenInterface {
    fn default() -> Self {
        Self {
            addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 6881,
        }
    }
}

// ---------------------------------------------------------------------------
// Config (the validated, owned value — not the builder)
// ---------------------------------------------------------------------------

/// Validated session configuration. Construct via [`SessionConfig::builder()`].
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub listen_interface: ListenInterface,
    /// Directory where downloaded files are saved.
    pub save_path: PathBuf,
    /// Base config directory (e.g., `~/.config/torrent-rs/`).
    pub config_dir: PathBuf,
    /// Directory for storing `.torrent` files (`config_dir/torrents/`).
    pub torrents_dir: PathBuf,

    // -- DHT --
    pub enable_dht: bool,
    /// DHT bootstrap nodes. None means "use DHT crate's defaults".
    pub dht_bootstrap_nodes: Option<Vec<BootstrapNode>>,

    // -- NAT traversal --
    /// Enable UPnP port mapping using igd_next.
    /// Attempts to automatically create port forwarding on the router.
    pub enable_port_mapping: bool,

    // -- Connection limits --
    /// Maximum concurrent peer connections per torrent.
    pub max_connections_per_torrent: NonZeroU32,
    /// Number of simultaneously unchoked peers.
    pub unchoke_slots: NonZeroU32,

    // -- Timeouts --
    pub peer_timeout: Duration,

    /// Override the process-wide peer ID.
    pub peer_id: Option<PeerID>,
}

impl SessionConfig {
    pub fn builder() -> SessionConfigBuilder {
        SessionConfigBuilder::default()
    }

    /// Convenience: derive the listen SocketAddr from the interface.
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_interface.socket_addr()
    }
}

// Keep a real Default so existing `SessionConfig::default()` call-sites in
// session.rs continue to compile without changes.
impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfigBuilder::default().build()
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SessionConfigBuilder {
    listen_interface: ListenInterface,
    save_path: PathBuf,
    config_dir: Option<PathBuf>,

    enable_dht: bool,
    dht_bootstrap_nodes: Option<Vec<BootstrapNode>>, // None = use defaults

    enable_port_mapping: bool,

    max_connections_per_torrent: NonZeroU32,
    unchoke_slots: NonZeroU32,
    peer_timeout: Duration,

    peer_id: Option<PeerID>,
}

impl Default for SessionConfigBuilder {
    fn default() -> Self {
        Self {
            listen_interface: ListenInterface::default(),
            save_path: PathBuf::from("."),
            config_dir: None,
            enable_dht: true,
            dht_bootstrap_nodes: None,
            enable_port_mapping: false,
            // SAFETY: literals are non-zero.
            max_connections_per_torrent: NonZeroU32::new(50).unwrap(),
            unchoke_slots: NonZeroU32::new(4).unwrap(),
            peer_timeout: Duration::from_secs(120),
            peer_id: None,
        }
    }
}

impl SessionConfigBuilder {
    pub fn listen_interface(mut self, iface: ListenInterface) -> Self {
        self.listen_interface = iface;
        self
    }

    pub fn save_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.save_path = path.into();
        self
    }

    pub fn config_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_dir = Some(path.into());
        self
    }

    pub fn enable_dht(mut self, enabled: bool) -> Self {
        self.enable_dht = enabled;
        self
    }

    /// Replace the bootstrap node list entirely.
    pub fn dht_bootstrap_nodes(mut self, nodes: Vec<BootstrapNode>) -> Self {
        self.dht_bootstrap_nodes = Some(nodes);
        self
    }

    /// Append a single bootstrap node to whatever list is already set
    /// (or to the defaults if none has been set yet).
    pub fn dht_bootstrap_node(mut self, host: impl Into<String>, port: u16) -> Self {
        self.dht_bootstrap_nodes
            .get_or_insert_with(default_dht_bootstrap_nodes)
            .push(BootstrapNode::new(host, port));
        self
    }

    pub fn enable_port_mapping(mut self, enabled: bool) -> Self {
        self.enable_port_mapping = enabled;
        self
    }

    pub fn max_connections_per_torrent(mut self, n: NonZeroU32) -> Self {
        self.max_connections_per_torrent = n;
        self
    }

    pub fn unchoke_slots(mut self, n: NonZeroU32) -> Self {
        self.unchoke_slots = n;
        self
    }

    pub fn peer_timeout(mut self, d: Duration) -> Self {
        self.peer_timeout = d;
        self
    }

    pub fn peer_id(mut self, id: PeerID) -> Self {
        self.peer_id = Some(id);
        self
    }

    pub fn build(self) -> SessionConfig {
        let config_dir = self.config_dir.unwrap_or_else(|| {
            ProjectDirs::from("com", "torrent-rs", "torrent-rs")
                .map(|d| d.config_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".config/torrent-rs"))
        });
        let torrents_dir = config_dir.join("torrents");

        SessionConfig {
            listen_interface: self.listen_interface,
            save_path: self.save_path,
            config_dir,
            torrents_dir,
            enable_dht: self.enable_dht,
            dht_bootstrap_nodes: self.dht_bootstrap_nodes,
            enable_port_mapping: self.enable_port_mapping,
            max_connections_per_torrent: self.max_connections_per_torrent,
            unchoke_slots: self.unchoke_slots,
            peer_timeout: self.peer_timeout,
            peer_id: self.peer_id,
        }
    }
}
