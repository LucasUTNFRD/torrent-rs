use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use bittorrent_common::{
    metainfo::{TorrentInfo, TorrentParseError, parse_torrent_from_file},
    types::{InfoHash, PeerID},
};
use bytes::BytesMut;
use magnet_uri::{Magnet, MagnetError};
use once_cell::sync::Lazy;
use peer_protocol::protocol::Handshake;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{
        mpsc::{self, UnboundedSender},
        watch,
    },
    task::JoinHandle,
};

use tracing::warn;
use tracker_client::TrackerHandler;

pub static CLIENT_ID: Lazy<PeerID> = Lazy::new(PeerID::generate);

use crate::{
    storage::Storage,
    torrent::{Torrent, TorrentError, TorrentMessage},
};

pub struct Session {
    pub handle: JoinHandle<()>,
    tx: UnboundedSender<SessionCommand>,
}

pub enum SessionCommand {
    AddTorrent(TorrentInfo),
    AddMagnet(Magnet),
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Magnet URI error: {0}")]
    Magnet(MagnetError),

    #[error("Torrent parsing error: {0}")]
    TorrentParse(#[from] TorrentParseError),
}

impl Session {
    pub fn new(port: u16, save_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let manager = SessionManager::new(port, save_path, rx);
        let handle = tokio::task::spawn(async move { manager.start().await });
        Self { tx, handle }
    }

    pub fn add_torrent(&self, dir: impl AsRef<Path>) -> Result<(), SessionError> {
        let metainfo = parse_torrent_from_file(dir.as_ref())?;
        let _ = self.tx.send(SessionCommand::AddTorrent(metainfo));
        Ok(())
    }

    pub fn add_magnet(&self, uri: impl AsRef<str>) -> Result<(), SessionError> {
        let magnet = Magnet::parse(uri).map_err(SessionError::Magnet)?;
        let _ = self.tx.send(SessionCommand::AddMagnet(magnet));
        Ok(())
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(SessionCommand::Shutdown);
    }
}

type TorrentSession = (
    mpsc::Sender<TorrentMessage>,
    JoinHandle<Result<(), TorrentError>>,
);

struct SessionManager {
    port: u16,
    save_path: PathBuf,
    rx: mpsc::UnboundedReceiver<SessionCommand>,
    sessions: Arc<RwLock<HashMap<InfoHash, TorrentSession>>>,
}

impl SessionManager {
    pub fn new(port: u16, save_path: PathBuf, rx: mpsc::UnboundedReceiver<SessionCommand>) -> Self {
        Self {
            port,
            save_path,
            rx,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Main entry point
    pub async fn start(mut self) {
        let tracker = Arc::new(TrackerHandler::new(*CLIENT_ID));
        let storage = Arc::new(Storage::new());

        let (shutdown_tx, shutdown_rx) = watch::channel(());

        self.spawn_tcp_listener();

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                SessionCommand::AddTorrent(metainfo) => {
                    tracing::info!(%metainfo);

                    let info_hash = metainfo.info_hash;
                    let (torrent, tx) = Torrent::from_torrent_info(
                        metainfo,
                        tracker.clone(),
                        storage.clone(),
                        shutdown_rx.clone(),
                    );

                    let session_handle = tokio::spawn(async move { torrent.start_session().await });

                    let t_session = (tx, session_handle);

                    let mut s = self.sessions.write().unwrap();

                    s.insert(info_hash, t_session);
                }
                SessionCommand::AddMagnet(magnet) => {
                    let info_hash = match magnet.info_hash() {
                        Some(ih) => ih,
                        None => {
                            tracing::error!("Magnet URI does not contain a valid info hash");
                            continue;
                        }
                    };

                    println!("{magnet}");
                    println!("Info Hash: {info_hash}");
                    if let Some(name) = &magnet.display_name {
                        println!("Display Name: {name}");
                    }
                    if magnet.trackers.is_empty() {
                        tracing::warn!(
                            "No tracker is specfied, client SHOULD use DHT to acquire peers",
                        );
                        tracing::warn!("Torrent-RS doest not implement DHT,yet");
                        continue;
                    }

                    println!("Trackers: {:?}", magnet.trackers);

                    println!("Peers: {:?}", magnet.peers);

                    let (torrent, tx) = Torrent::from_magnet(
                        magnet,
                        tracker.clone(),
                        storage.clone(),
                        shutdown_rx.clone(),
                    );

                    let session_handle = tokio::spawn(async move { torrent.start_session().await });

                    let t_session = (tx, session_handle);

                    let mut s = self.sessions.write().unwrap();

                    s.insert(info_hash, t_session);
                }
                SessionCommand::Shutdown => {
                    if shutdown_tx.send(()).is_err() {
                        tracing::warn!("No receivers for shutdown signal");
                    }

                    let handles: Vec<_> = {
                        let mut sessions = self.sessions.write().unwrap();
                        sessions.drain().map(|(_, (_, h))| h).collect()
                    };

                    for h in handles {
                        match h.await {
                            Ok(Ok(())) => tracing::info!("Session exited cleanly"),
                            Ok(Err(e)) => tracing::warn!(?e, "Session error"),
                            Err(e) => tracing::warn!(?e, "Join error"),
                        }
                    }

                    break;
                }
            }
        }
    }

    fn spawn_tcp_listener(&self) {
        tracing::info!("started connection listener");
        let port = self.port;
        let torrent = self.sessions.clone();

        // make this a spawn peer connection
        // and after reading info hash, perform a attact_to_torrent
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

                    // torrent
                    //     .read()
                    //     .unwrap()
                    //     .get(&remote_handshake.info_hash)
                    //     .unwrap()
                    //     .add_peer(torrent::Peer::Inbound {
                    //         stream,
                    //         remote_addr,
                    //         supports_ext,
                    //     });
                });
            }
        });
    }
}
