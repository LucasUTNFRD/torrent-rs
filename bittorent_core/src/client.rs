use std::collections::HashMap;

use crate::{
    tracker::TrackerHandler,
    types::{InfoHash, PeerID},
    util::generate_peer_id,
};

pub const PORT: u16 = 6881;

pub struct TorrentSession {}

pub struct BittorentClient {
    peer_id: PeerID,
    port: u16,
    torrents: HashMap<InfoHash, TorrentSession>,
    // tracker_service: TrackerService,
    // event_tx: mpsc::UnboundedSender<ClientEvent>,
    // event_rx: Option<mpsc::UnboundedReceiver<ClientEvent>>,
    // shutdown_tx: broadcast::Sender<()>,
}

impl BittorentClient {
    #[allow(clippy::new_without_default)]
    pub fn new(/*load some user config*/) -> Self {
        let peer_id = generate_peer_id();
        Self {
            peer_id,
            port: PORT,
            torrents: HashMap::new(),
        }
    }

    pub fn start(&self) {
        // Each torrent get its own tracker supervisor but SHARES the tracker manager
        let tracker_manager = TrackerHandler::new(self.peer_id);
    }
}

pub enum ClientCommand {
    AddTorrent,
}
