use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use bittorrent_common::{
    metainfo::parse_torrent_from_file,
    types::{InfoHash, PeerID},
};
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tracker_client::TrackerHandler;

use crate::{
    storage::Storage,
    torrent::{TorrentSession, TorrentStats},
};

pub struct Session {
    pub handle: JoinHandle<()>,
    tx: UnboundedSender<SessionCommand>,
    pub stats_receiver: UnboundedReceiver<TorrentStats>,
}

pub enum SessionCommand {
    AddTorrent { directory: PathBuf },
    Shutdown,
}

impl Session {
    pub fn new(port: u16, save_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (stats_tx, statx_rx) = mpsc::unbounded_channel();

        let manager = SessionManager::new(port, save_path, rx, stats_tx);
        let handle = tokio::task::spawn(async move { manager.start().await });
        Self {
            tx,
            handle,
            stats_receiver: statx_rx,
        }
        // Self { port, save_path }
    }

    pub fn add_torrent(&self, dir: impl AsRef<Path>) {
        let _ = self.tx.send(SessionCommand::AddTorrent {
            directory: dir.as_ref().to_path_buf(),
        });
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(SessionCommand::Shutdown);
    }
}

#[allow(dead_code)]
struct SessionManager {
    port: u16,
    save_path: PathBuf,
    rx: mpsc::UnboundedReceiver<SessionCommand>,
    torrents: HashMap<InfoHash, TorrentSession>,
    stats_tx: mpsc::UnboundedSender<TorrentStats>,
}

impl SessionManager {
    pub fn new(
        port: u16,
        save_path: PathBuf,
        rx: mpsc::UnboundedReceiver<SessionCommand>,
        stats_tx: UnboundedSender<TorrentStats>,
    ) -> Self {
        Self {
            port,
            save_path,
            rx,
            torrents: HashMap::new(),
            stats_tx,
        }
    }

    pub async fn start(mut self) {
        let client_id = PeerID::generate();
        let tracker = Arc::new(TrackerHandler::new(client_id));
        let storage = Arc::new(Storage::new());

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                SessionCommand::AddTorrent { directory } => {
                    let metainfo = match parse_torrent_from_file(directory) {
                        Ok(torrent) => torrent,
                        Err(e) => {
                            tracing::warn!(?e);
                            return;
                        }
                    };

                    tracing::info!(%metainfo);

                    let info_hash = metainfo.info_hash;
                    let torrent_session = TorrentSession::new(
                        metainfo,
                        tracker.clone(),
                        client_id,
                        storage.clone(),
                        self.stats_tx.clone(),
                    );

                    self.torrents.insert(info_hash, torrent_session);
                }
                SessionCommand::Shutdown => {
                    for (info, torrent_session) in self.torrents.iter() {
                        tracing::debug!(%info,"Shutdown ");
                        torrent_session.shutdown();
                    }
                }
            }
        }
    }
}
