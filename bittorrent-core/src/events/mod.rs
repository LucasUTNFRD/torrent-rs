use tokio::sync::broadcast;

pub mod peer;
pub mod session;
pub mod torrent;

pub use peer::PeerEvent;
pub use session::SessionEvent;
pub use torrent::TorrentEvent;

pub const TORRENT_EVENT_CAPACITY: usize = 256;
pub const PEER_EVENT_CAPACITY: usize = 512;
pub const SESSION_EVENT_CAPACITY: usize = 64;

#[derive(Clone)]
pub struct EventBus {
    pub torrent_tx: broadcast::Sender<TorrentEvent>,
    pub peer_tx: broadcast::Sender<PeerEvent>,
    pub session_tx: broadcast::Sender<SessionEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            torrent_tx: broadcast::channel(TORRENT_EVENT_CAPACITY).0,
            peer_tx: broadcast::channel(PEER_EVENT_CAPACITY).0,
            session_tx: broadcast::channel(SESSION_EVENT_CAPACITY).0,
        }
    }

    pub fn subscribe_torrent(&self) -> broadcast::Receiver<TorrentEvent> {
        self.torrent_tx.subscribe()
    }

    pub fn subscribe_peer(&self) -> broadcast::Receiver<PeerEvent> {
        self.peer_tx.subscribe()
    }

    pub fn subscribe_session(&self) -> broadcast::Receiver<SessionEvent> {
        self.session_tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
