use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use bittorrent_common::{
    metainfo::parse_torrent_from_file,
    types::{InfoHash, PeerID},
};
use bytes::BytesMut;
use once_cell::sync::Lazy;
use peer_protocol::protocol::Handshake;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tracker_client::TrackerHandler;

pub static CLIENT_ID: Lazy<PeerID> = Lazy::new(PeerID::generate);

use crate::{
    storage::Storage,
    torrent::{self, TorrentSession, TorrentStats},
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
    torrents: Arc<RwLock<HashMap<InfoHash, TorrentSession>>>,
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
            torrents: Arc::new(RwLock::new(HashMap::new())),
            stats_tx,
        }
    }

    /// Main entry point
    pub async fn start(mut self) {
        let tracker = Arc::new(TrackerHandler::new(*CLIENT_ID));
        let storage = Arc::new(Storage::new());

        self.spawn_tcp_listener();

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
                        storage.clone(),
                        self.stats_tx.clone(),
                    );

                    let mut torrent_guard = self.torrents.write().unwrap();
                    torrent_guard.insert(info_hash, torrent_session);
                }
                SessionCommand::Shutdown => {
                    let torrent_guard = self.torrents.read().unwrap();
                    for (info, torrent_session) in torrent_guard.iter() {
                        tracing::debug!(%info,"Shutdown ");
                        torrent_session.shutdown();
                    }
                }
            }
        }
    }

    // TODO: Gracefully stop tcp_listener
    fn spawn_tcp_listener(&self) {
        tracing::info!("started connection listener");
        let port = self.port;
        let torrent = self.torrents.clone();

        tokio::spawn(async move {
            let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
                .await
                .expect("failed to bind tcp listener");
            tracing::info!("{:?}", listener.local_addr());

            while let Ok((mut stream, remote_addr)) = listener.accept().await {
                tracing::info!("accepted connection from {:?}", remote_addr);
                let torrent = torrent.clone();
                tokio::spawn(async move {
                    let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);

                    if let Err(e) = stream.read_exact(&mut buf).await {
                        tracing::error!(error = ?e,"Failed to read handshake from {:?} ", remote_addr,);
                        return;
                    }

                    let remote_handshake = match Handshake::from_bytes(&buf) {
                        Some(hs) => hs,
                        None => {
                            tracing::error!("Failed to parse handshake from {:?} ", remote_addr,);
                            return;
                        }
                    };

                    let have_torrent = {
                        torrent
                            .read()
                            .unwrap()
                            .contains_key(&remote_handshake.info_hash)
                    };

                    if !have_torrent {
                        return;
                    }

                    let handshake = Handshake::new(*CLIENT_ID, remote_handshake.info_hash);
                    if let Err(e) = stream.write_all(&handshake.to_bytes()).await {
                        tracing::error!(error = ?e, "Failed to send handshake to {:?}", remote_addr);
                        return;
                    }

                    let supports_ext = remote_handshake.support_extended_message();

                    tracing::info!(
                        "add connection from {:?} for info_hash :{:?}",
                        remote_addr,
                        remote_handshake.info_hash
                    );

                    torrent
                        .read()
                        .unwrap()
                        .get(&remote_handshake.info_hash)
                        .unwrap()
                        .add_peer(torrent::Peer::Inbound {
                            stream,
                            remote_addr,
                            supports_ext,
                        });
                });
            }
        });
    }
}
