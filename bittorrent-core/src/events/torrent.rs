use crate::metrics::progress::TorrentState;

#[derive(Debug, Clone)]
pub enum TorrentEvent {
    StateChanged {
        prev: TorrentState,
        next: TorrentState,
    },
    TorrentFinished,
    TorrentPaused,
    TorrentResumed,
    FileCompleted {
        file_index: u32,
    },
    TrackerAnnounced {
        url: String,
        peers_received: u32,
    },
    TrackerError {
        url: String,
        error: String,
        times_in_row: u32,
    },
    HashFailed {
        piece_index: u32,
    },
    MetadataReceived,
}
