#[derive(Debug, Clone)]
pub struct TorrentProgress {
    pub name: String,
    pub total_pieces: u32,
    pub verified_pieces: u32,
    pub failed_pieces: u32,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub connected_peers: u32,
    pub download_rate: f64, // bytes/sec, computed by session on interval
    pub upload_rate: f64,
    pub state: TorrentState,
    pub eta_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentState {
    FetchingMetadata,
    Checking,
    Downloading,
    Seeding,
    Paused,
    Error(String),
    Finished,
}

impl Default for TorrentProgress {
    fn default() -> Self {
        Self {
            name: String::new(),
            total_pieces: 0,
            verified_pieces: 0,
            failed_pieces: 0,
            total_bytes: 0,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            connected_peers: 0,
            download_rate: 0.0,
            upload_rate: 0.0,
            state: TorrentState::Checking,
            eta_seconds: None,
        }
    }
}
