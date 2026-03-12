use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use bittorrent_common::types::PeerID;
use directories::ProjectDirs;

/// Configuration for creating a new Session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub listen_addr: SocketAddr,
    /// Directory where downloaded files are saved
    pub save_path: PathBuf,
    /// Base config directory (e.g., ~/.config/torrent-rs/)
    pub config_dir: PathBuf,
    /// Directory for storing .torrent files (config_dir/torrents/)
    pub torrents_dir: PathBuf,

    /// Whether to enable DHT for peer discovery
    pub enable_dht: bool,
    pub max_connections_per_torrent: u32,
    pub unchoke_slots_limit: u32,
    /// How long to wait before dropping a stalled peer (test_timeout.cpp)
    pub peer_timeout: std::time::Duration,

    /// Custom peer ID for this session. If None, a global one is used.
    pub peer_id: Option<PeerID>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        let config_dir = ProjectDirs::from("com", "torrent-rs", "torrent-rs")
            .map(|dirs| dirs.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".config/torrent-rs"));

        let torrents_dir = config_dir.join("torrents");

        Self {
            listen_addr: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED), 6881),
            save_path: PathBuf::from("."),
            enable_dht: true,
            config_dir,
            torrents_dir,
            unchoke_slots_limit: 4,
            max_connections_per_torrent: 50,
            peer_timeout: Duration::from_secs(120),
            peer_id: None,
        }
    }
}
