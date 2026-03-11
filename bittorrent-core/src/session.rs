//! Session management for the ``BitTorrent`` daemon.
//!
//! The `Session` struct provides the public API for controlling the daemon,
//! while `SessionManager` runs as a background task handling commands.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, RwLock},
};

use bittorrent_common::{
    metainfo::{TorrentInfo, TorrentParseError, parse_torrent_from_file},
    types::{InfoHash, PeerID},
};
use bytes::BytesMut;
use magnet_uri::{Magnet, MagnetError};
use peer_protocol::protocol::Handshake;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{
        broadcast,
        mpsc::{self, UnboundedSender},
        oneshot, watch,
    },
    task::JoinHandle,
};

use mainline_dht::{DhtConfig, DhtHandler};
use tracker_client::TrackerHandler;

use crate::{
    SessionConfig,
    net::TcpListener,
    storage::{DiskStorage, StorageBackend},
    torrent::{Torrent, TorrentError, TorrentMessage},
    types::{TorrentId, SessionEvent, TorrentMetrics},
    verify_torrent_file::verify_content,
};

/// Global client peer ID, generated once per process.
pub static CLIENT_ID: LazyLock<PeerID> = LazyLock::new(PeerID::generate);

/// Handle to a running ``BitTorrent`` session.
///
/// This is the main entry point for interacting with the daemon.
/// All methods are async and return results via oneshot channels.
pub struct Session {
    /// Handle to the background ``SessionManager`` task
    pub handle: JoinHandle<()>,
    event_rx: broadcast::Receiver<SessionEvent>,
    /// Channel for sending commands to the ``SessionManager``
    tx: UnboundedSender<SessionCommand>,
}

pub struct SessionBuilder {
    config: SessionConfig,
    storage: Option<Arc<dyn StorageBackend>>,
}

impl SessionBuilder {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            storage: None,
        }
    }

    pub fn build(self) -> Session {
        let (tx, rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = broadcast::channel(1024);

        let storage = self.storage.unwrap_or_else(|| Arc::new(DiskStorage::new()));

        let manager = SessionManager::new(self.config, rx, storage, event_tx);

        let handle = tokio::task::spawn(async move { manager.start().await });

        Session {
            handle,
            event_rx,
            tx,
        }
    }
}

/// Commands sent from Session to ``SessionManager``.
pub enum SessionCommand {
    AddTorrent {
        info: TorrentInfo,
        resp: oneshot::Sender<Result<TorrentId, SessionError>>,
    },
    AddMagnet {
        magnet: Magnet,
        resp: oneshot::Sender<Result<TorrentId, SessionError>>,
    },
    RemoveTorrent {
        id: TorrentId,
        resp: oneshot::Sender<Result<(), SessionError>>,
    },
    SeedFile {
        info: TorrentInfo,
        content_dir: PathBuf,
        resp: oneshot::Sender<Result<TorrentId, SessionError>>,
    },
    GetMetrics {
        id: TorrentId,
        resp: oneshot::Sender<Result<watch::Receiver<TorrentMetrics>, SessionError>>,
    },
    Shutdown {
        resp: oneshot::Sender<Result<(), SessionError>>,
    },
}

/// Errors that can occur in session operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Magnet URI error: {0}")]
    Magnet(#[from] MagnetError),

    #[error("Torrent parsing error: {0}")]
    TorrentParse(#[from] TorrentParseError),

    #[error("Session closed")]
    SessionClosed,

    #[error("Torrent not found: {0}")]
    TorrentNotFound(TorrentId),

    #[error("Torrent already exists: {0}")]
    TorrentAlreadyExists(TorrentId),

    #[error("Invalid magnet URI: missing info hash")]
    InvalidMagnet,

    #[error("No peer discovery available: no trackers and DHT disabled")]
    NoPeerDiscovery,
}

impl Session {
    /// Create a new session with the given configuration.
    ///
    /// This spawns a background task that manages torrents and handles commands.
    pub fn new(config: SessionConfig) -> Self {
        SessionBuilder::new(config).build()
    }

    pub fn builder(config: SessionConfig) -> SessionBuilder {
        SessionBuilder::new(config)
    }

    /// Add a torrent from a .torrent file.
    ///
    /// Returns the ``TorrentId`` on success.
    pub async fn add_torrent(&self, path: impl AsRef<Path>) -> Result<TorrentId, SessionError> {
        let metainfo = parse_torrent_from_file(path.as_ref())?;
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::AddTorrent {
                info: metainfo,
                resp: tx,
            })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    /// Add a torrent from a magnet URI.
    ///
    /// Returns the ``TorrentId``  on success.
    pub async fn add_magnet(&self, uri: impl AsRef<str>) -> Result<TorrentId, SessionError> {
        let magnet = Magnet::parse(uri)?;
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::AddMagnet { magnet, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    /// Remove a torrent from the session.
    ///
    /// This stops the torrent but does not delete downloaded files.
    pub async fn remove_torrent(&self, id: TorrentId) -> Result<(), SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::RemoveTorrent { id, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    /// Get information abo
    pub async fn seed_torrent(
        &self,
        info: TorrentInfo,
        content_dir: PathBuf,
    ) -> Result<TorrentId, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::SeedFile {
                info,
                content_dir,
                resp: tx,
            })
            .map_err(|_| SessionError::SessionClosed)?;

        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    /// Shutdown the session gracefully.
    ///
    /// This stops all torrents and waits for them to complete.
    pub async fn shutdown(&self) -> Result<(), SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::Shutdown { resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    /// Subscribe to global session events.
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_rx.resubscribe()
    }

    /// Subscribe to metrics for a specific torrent.
    pub async fn subscribe_torrent(
        &self,
        id: TorrentId,
    ) -> Result<watch::Receiver<TorrentMetrics>, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::GetMetrics { id, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    pub async fn wait_for_completion(&self, id: TorrentId) -> Result<(), SessionError> {
        let mut rx = self.subscribe();
        while let Ok(event) = rx.recv().await {
            match event {
                SessionEvent::TorrentCompleted(completed_id) if completed_id == id => return Ok(()),
                SessionEvent::TorrentRemoved(removed_id) if removed_id == id => {
                    return Err(SessionError::TorrentNotFound(id));
                }
                _ => {}
            }
        }
        Err(SessionError::SessionClosed)
    }

    pub fn suscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.subscribe()
    }
}

/// Metadata stored for each active torrent.
struct TorrentEntry {
    /// Channel to send messages to the torrent task
    tx: mpsc::Sender<TorrentMessage>,
    /// Handle to the torrent task
    handle: JoinHandle<Result<(), TorrentError>>,
    /// Metrics receiver for the torrent
    metrics_rx: watch::Receiver<TorrentMetrics>,
    /// Display name of the torrent
    name: String,
    /// Total size in bytes (0 if metadata not yet fetched)
    size_bytes: u64,
}

/// Internal session manager that runs as a background task.
struct SessionManager {
    config: SessionConfig,
    event_tx: broadcast::Sender<SessionEvent>,
    rx: mpsc::UnboundedReceiver<SessionCommand>,
    sessions: Arc<RwLock<HashMap<InfoHash, TorrentEntry>>>,
    storage: Arc<dyn StorageBackend>,
}

impl SessionManager {
    pub fn new(
        config: SessionConfig,
        rx: mpsc::UnboundedReceiver<SessionCommand>,
        storage: Arc<dyn StorageBackend>,
        event_tx: broadcast::Sender<SessionEvent>,
    ) -> Self {
        Self {
            config,
            event_tx,
            rx,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            storage,
        }
    }

    /// Main entry point - runs the session manager loop.
    pub async fn start(mut self) {
        let tracker = Arc::new(TrackerHandler::new(*CLIENT_ID));

        // TODO: FOr now im ignoring the config builder of the DHT
        // but some ke feature we will need is to give a list of nodes as boostrap source
        // also move dht to core
        let dht: Option<Arc<DhtHandler>> = if self.config.enable_dht {
            let config_dir = &self.config.config_dir;
            let dht_config = DhtConfig {
                id_file_path: Some(config_dir.join("node.id")),
                state_file_path: Some(config_dir.join("dht_state.dat")),
                port: self.config.listen_addr.port(),
            };

            match DhtHandler::with_config(dht_config).await {
                Ok(dht) => {
                    tracing::info!("DHT node created, bootstrapping...");
                    match dht.bootstrap().await {
                        Ok(node_id) => {
                            tracing::info!(
                                "DHT bootstrapped successfully with node ID: {:?}",
                                node_id
                            );
                            Some(Arc::new(dht))
                        }
                        Err(e) => {
                            tracing::warn!("DHT bootstrap failed: {}, continuing without DHT", e);
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to create DHT node: {}, continuing without DHT", e);
                    None
                }
            }
        } else {
            tracing::info!("DHT disabled by configuration");
            None
        };

        // Ensure torrents directory exists
        if let Err(e) = std::fs::create_dir_all(&self.config.torrents_dir) {
            tracing::warn!(
                "Failed to create torrents directory {}: {}",
                self.config.torrents_dir.display(),
                e
            );
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(());

        self.spawn_tcp_listener();

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                SessionCommand::AddTorrent { info, resp } => {
                    let result =
                        self.handle_add_torrent(info, &tracker, dht.as_ref(), &shutdown_rx);
                    let _ = resp.send(result);
                }
                SessionCommand::AddMagnet { magnet, resp } => {
                    let result =
                        self.handle_add_magnet(magnet, &tracker, dht.as_ref(), &shutdown_rx);
                    let _ = resp.send(result);
                }
                SessionCommand::SeedFile {
                    info,
                    content_dir,
                    resp,
                } => {
                    // 1. Validate metadate from .torrent with  content_dir
                    let is_valid = verify_content(&content_dir, &info).unwrap();

                    let result = if is_valid {
                        self.handle_seed_torrent(
                            info,
                            content_dir,
                            &tracker,
                            dht.as_ref(),
                            &shutdown_rx,
                        )
                        .await
                    } else {
                        // TODO: Given metadata did not match with contents
                        todo!("Return a proper session error");
                    };

                    let _ = resp.send(result);
                }
                SessionCommand::RemoveTorrent { id, resp } => {
                    let result = self.handle_remove_torrent(id).await;
                    let _ = resp.send(result);
                }
                SessionCommand::GetMetrics { id, resp } => {
                    let result = {
                        let sessions = self.sessions.read().unwrap();
                        sessions
                            .get(&id)
                            .map(|entry| entry.metrics_rx.clone())
                            .ok_or(SessionError::TorrentNotFound(id))
                    };
                    let _ = resp.send(result);
                }

                SessionCommand::Shutdown { resp } => {
                    let result = self.handle_shutdown(&shutdown_tx, dht.as_ref()).await;
                    let _ = resp.send(result);
                    break;
                }
            }
        }
    }

    async fn handle_seed_torrent(
        &self,
        metainfo: TorrentInfo,
        content_dir: PathBuf,
        tracker: &Arc<TrackerHandler>,
        dht: Option<&Arc<DhtHandler>>,
        shutdown_rx: &watch::Receiver<()>,
    ) -> Result<TorrentId, SessionError> {
        let info_hash = metainfo.info_hash;

        // Check for duplicates
        {
            let sessions = self.sessions.read().unwrap();
            if sessions.contains_key(&info_hash) {
                return Err(SessionError::TorrentAlreadyExists(info_hash));
            }
        }

        tracing::info!(%metainfo);

        let name = metainfo.info.mode.name().to_string();
        let size_bytes = u64::try_from(metainfo.info.total_size()).expect("size is non-negative");

        let (torrent, tx, metrics_rx) = Torrent::as_seed(
            content_dir,
            metainfo,
            tracker.clone(),
            dht.cloned(),
            self.storage.clone(),
            shutdown_rx.clone(),
            self.config.torrents_dir.clone(),
            self.event_tx.clone(),
        );

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            metrics_rx,
            name,
            size_bytes,
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

        let _ = self.event_tx.send(SessionEvent::TorrentAdded(info_hash));

        Ok(info_hash)
    }

    fn handle_add_torrent(
        &self,
        metainfo: TorrentInfo,
        tracker: &Arc<TrackerHandler>,
        dht: Option<&Arc<DhtHandler>>,
        shutdown_rx: &watch::Receiver<()>,
    ) -> Result<TorrentId, SessionError> {
        let info_hash = metainfo.info_hash;

        // Check for duplicates
        {
            let sessions = self.sessions.read().unwrap();
            if sessions.contains_key(&info_hash) {
                return Err(SessionError::TorrentAlreadyExists(info_hash));
            }
        }

        tracing::info!(%metainfo);

        let name = metainfo.info.mode.name().to_string();
        let size_bytes = u64::try_from(metainfo.info.total_size()).expect("size is non-negative");

        let (torrent, tx, metrics_rx) = Torrent::from_torrent_info(
            metainfo,
            tracker.clone(),
            dht.cloned(),
            self.storage.clone(),
            shutdown_rx.clone(),
            self.config.torrents_dir.clone(),
            self.event_tx.clone(),
        );

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            metrics_rx,
            name,
            size_bytes,
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

        let _ = self.event_tx.send(SessionEvent::TorrentAdded(info_hash));

        Ok(info_hash)
    }

    fn handle_add_magnet(
        &self,
        magnet: Magnet,
        tracker: &Arc<TrackerHandler>,
        dht: Option<&Arc<DhtHandler>>,
        shutdown_rx: &watch::Receiver<()>,
    ) -> Result<TorrentId, SessionError> {
        let info_hash = magnet.info_hash().ok_or(SessionError::InvalidMagnet)?;

        // Check for duplicates
        {
            let sessions = self.sessions.read().unwrap();
            if sessions.contains_key(&info_hash) {
                return Err(SessionError::TorrentAlreadyExists(info_hash));
            }
        }

        // Check if we have any way to discover peers
        if magnet.trackers.is_empty() && dht.is_none() {
            return Err(SessionError::NoPeerDiscovery);
        }

        tracing::info!("Adding magnet: {}", magnet);
        tracing::info!("Info Hash: {}", info_hash);
        if let Some(name) = &magnet.display_name {
            tracing::info!("Display Name: {}", name);
        }

        let name = magnet
            .display_name
            .clone()
            .unwrap_or_else(|| info_hash.to_string());

        let (torrent, tx, metrics_rx) = Torrent::from_magnet(
            magnet,
            tracker.clone(),
            dht.cloned(),
            self.storage.clone(),
            shutdown_rx.clone(),
            self.config.torrents_dir.clone(),
            self.event_tx.clone(),
        );

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            metrics_rx,
            name,
            size_bytes: 0, // Unknown until metadata is fetched
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

        let _ = self.event_tx.send(SessionEvent::TorrentAdded(info_hash));

        Ok(info_hash)
    }

    async fn handle_remove_torrent(&self, id: TorrentId) -> Result<(), SessionError> {
        let entry = {
            let mut sessions = self.sessions.write().unwrap();
            sessions.remove(&id)
        };

        match entry {
            Some(entry) => {
                // The torrent task should exit when its channel is dropped
                drop(entry.tx);
                // Wait for it to finish
                match entry.handle.await {
                    Ok(Ok(())) => tracing::info!("Torrent {} removed cleanly", id),
                    Ok(Err(e)) => tracing::warn!("Torrent {} error on removal: {:?}", id, e),
                    Err(e) => tracing::warn!("Torrent {} join error: {:?}", id, e),
                }
                let _ = self.event_tx.send(SessionEvent::TorrentRemoved(id));
                Ok(())
            }
            None => Err(SessionError::TorrentNotFound(id)),
        }
    }

    async fn handle_shutdown(
        &self,
        shutdown_tx: &watch::Sender<()>,
        dht: Option<&Arc<DhtHandler>>,
    ) -> Result<(), SessionError> {
        // Signal shutdown to all torrents
        if shutdown_tx.send(()).is_err() {
            tracing::warn!("No receivers for shutdown signal");
        }

        // Collect and wait for all torrent handles
        let handles: Vec<_> = {
            let mut sessions = self.sessions.write().unwrap();
            sessions.drain().map(|(_, entry)| entry.handle).collect()
        };

        for h in handles {
            match h.await {
                Ok(Ok(())) => tracing::info!("Torrent exited cleanly"),
                Ok(Err(e)) => tracing::warn!(?e, "Torrent error"),
                Err(e) => tracing::warn!(?e, "Join error"),
            }
        }

        // Shutdown DHT gracefully
        if let Some(dht) = dht {
            tracing::info!("Shutting down DHT...");
            if let Err(e) = dht.shutdown().await {
                tracing::warn!("DHT shutdown error: {}", e);
            } else {
                tracing::info!("DHT shutdown complete");
            }
        }

        Ok(())
    }

    fn spawn_tcp_listener(&self) {
        let sessions = self.sessions.clone();
        let listen_addr = self.config.listen_addr;

        tokio::spawn(async move {
            let listener = TcpListener::bind(listen_addr)
                .await
                .expect("failed to bind tcp listener");
            tracing::info!("Listening on {:?}", listener.local_addr());

            while let Ok((mut stream, remote_addr)) = listener.accept().await {
                tracing::debug!("Accepted connection from {:?}", remote_addr);
                let sessions = sessions.clone();

                tokio::spawn(async move {
                    let mut buf = BytesMut::zeroed(Handshake::HANDSHAKE_LEN);

                    if let Err(e) = stream.read_exact(&mut buf).await {
                        tracing::debug!(
                            error = ?e,
                            "Failed to read handshake from {:?}",
                            remote_addr
                        );
                        return;
                    }

                    let Some(remote_handshake) = Handshake::from_bytes(&buf) else {
                        tracing::debug!("Failed to parse handshake from {:?}", remote_addr);
                        return;
                    };

                    let have_torrent = {
                        sessions
                            .read()
                            .unwrap()
                            .contains_key(&remote_handshake.info_hash)
                    };

                    if !have_torrent {
                        tracing::debug!(
                            "Rejecting connection for unknown torrent {:?}",
                            remote_handshake.info_hash
                        );
                        return;
                    }

                    let handshake = Handshake::new(*CLIENT_ID, remote_handshake.info_hash);
                    if let Err(e) = stream.write_all(&handshake.to_bytes()).await {
                        tracing::debug!(
                            error = ?e,
                            "Failed to send handshake to {:?}",
                            remote_addr
                        );
                        return;
                    }

                    let supports_ext = remote_handshake.support_extended_message();

                    tracing::info!(
                        "Incoming peer connection from {:?} for {:?}",
                        remote_addr,
                        remote_handshake.info_hash
                    );

                    // Forward connection to the appropriate torrent
                    let torrent_tx = {
                        sessions
                            .read()
                            .unwrap()
                            .get(&remote_handshake.info_hash)
                            .map(|entry| entry.tx.clone())
                    };

                    if let Some(tx) = torrent_tx {
                        let _ = tx
                            .send(TorrentMessage::InboundPeer {
                                stream,
                                remote_addr,
                                supports_ext,
                                peer_id: remote_handshake.peer_id,
                            })
                            .await;
                    }
                });
            }
        });
    }
}
