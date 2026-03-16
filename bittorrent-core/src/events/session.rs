use std::net::{IpAddr, SocketAddr};
use bittorrent_common::types::InfoHash;

#[derive(Debug, Clone)]
pub enum SessionEvent {
    TorrentAdded(InfoHash),
    TorrentRemoved(InfoHash),
    TorrentCompleted(InfoHash),
    MetadataFetched(InfoHash),
    TorrentError(InfoHash, String),

    ListenSucceeded { addr: SocketAddr },
    ListenFailed    { addr: SocketAddr, error: String },
    ExternalIpDiscovered { addr: IpAddr },
}
