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
    sync::mpsc::{self, UnboundedSender},
    task::JoinHandle,
};
use tracker_client::TrackerHandler;

use crate::torrent::TorrentSession;

pub struct Session {
    pub handle: JoinHandle<()>,
    tx: UnboundedSender<SessionCommand>,
}

pub enum SessionCommand {
    AddTorrent { directory: PathBuf },
    Shutdown,
}

impl Session {
    pub fn new(port: u16, save_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let manager = SessionManager::new(port, save_path, rx);
        let handle = tokio::task::spawn(async move { manager.start().await });
        Self { tx, handle }
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

struct SessionManager {
    port: u16,
    save_path: PathBuf,
    rx: mpsc::UnboundedReceiver<SessionCommand>,
    torrents: HashMap<InfoHash, TorrentSession>,
}

impl SessionManager {
    pub fn new(port: u16, save_path: PathBuf, rx: mpsc::UnboundedReceiver<SessionCommand>) -> Self {
        Self {
            port,
            save_path,
            rx,
            torrents: HashMap::new(),
        }
    }

    pub async fn start(mut self) {
        let client_id = PeerID::generate();
        let tracker = Arc::new(TrackerHandler::new(client_id));
        // TODO: Implement storage abstraction
        // let disk_io = Disk::new():

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                SessionCommand::AddTorrent { directory } => {
                    // WARN: This call is blocking
                    let metainfo = match parse_torrent_from_file(directory) {
                        Ok(torrent) => torrent,
                        Err(e) => {
                            // TODO:: Handle error?
                            tracing::warn!(?e);
                            return;
                        }
                    };

                    tracing::info!(%metainfo);

                    let info_hash = metainfo.info_hash;
                    let torrent_session = TorrentSession::new(metainfo, tracker.clone(), client_id);

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
