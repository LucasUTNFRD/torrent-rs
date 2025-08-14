use crate::types::InfoHash;

pub struct TorrentSession {
    pub info_hash: InfoHash,
    // pub torrent_file: TorrentFile,
    // pub state: TorrentState,
    // pub stats: TorrentStats,
    // pub save_path: PathBuf,
    // pub peer_id: PeerId,
    pub port: u16,
    pub last_announce: Option<std::time::Instant>,
}
