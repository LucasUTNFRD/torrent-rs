use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize},
};

use bittorrent_common::{metainfo::Info, types::InfoHash};
use tokio::sync::watch;
use tracker_client::TrackerHandler;

use crate::Storage;
//
// Metrics Control Structure
//

pub struct Metrics {
    pub downloaded_bytes: AtomicU64,
    pub uploaded_bytes: AtomicU64,
    pub connected_peers: AtomicUsize,
}

//
// TORRENT Control Structure
//

/// Torrent Struct for individual torrent files
pub struct Torrent {
    /// None indicates that the torrent is from a MagnetURI so we need to fetch metadata from
    /// peers via ut_metadata extension
    /// Some(Info) indicates the torrent is from .torrent file so we need to fetch peer from
    /// tracker and run as stated in BEP 0003
    pub info: Option<Info>,
    pub state: State,
    pub info_hash: InfoHash,
    pub trackers: Vec<String>,
    pub tracker: Arc<TrackerHandler>,
    pub storage: Arc<Storage>,
    pub metrics: Arc<Metrics>,
    /// Shutdown signal
    shutdown_tx: watch::Sender<bool>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum State {
    Seeding,
    Paused,
    Leeching,
}

impl Torrent {
    pub fn new() -> Self {
        todo!()
    }
}
