//! Session management for the ``BitTorrent`` daemon.
//!
//! The `Session` struct provides the public API for controlling the daemon,
//! while `SessionManager` runs as a background task handling commands.

use std::{
    collections::HashMap,
    path::Path,
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
    net::TcpListener,
    sync::{
        mpsc::{self, UnboundedSender},
        oneshot, watch,
    },
    task::JoinHandle,
};

use mainline_dht::{DhtConfig, DhtHandler};
use tracker_client::TrackerHandler;

use crate::{
    storage::Storage,
    torrent::{Torrent, TorrentError, TorrentMessage},
    types::{SessionConfig, SessionStats, TorrentId, TorrentState, TorrentSummary},
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
    /// Channel for sending commands to the ``SessionManager``
    tx: UnboundedSender<SessionCommand>,
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
    ListTorrents {
        resp: oneshot::Sender<Vec<TorrentSummary>>,
    },
    GetTorrent {
        id: TorrentId,
        resp: oneshot::Sender<Option<TorrentSummary>>,
    },
    GetStats {
        resp: oneshot::Sender<SessionStats>,
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
    #[must_use]
    pub fn new(config: SessionConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let manager = SessionManager::new(config, rx);
        let handle = tokio::task::spawn(async move { manager.start().await });
        Self { handle, tx }
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

    /// List all torrents in the session.
    pub async fn list_torrents(&self) -> Result<Vec<TorrentSummary>, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::ListTorrents { resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)
    }

    /// Get information about a specific torrent.
    pub async fn get_torrent(&self, id: TorrentId) -> Result<Option<TorrentSummary>, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::GetTorrent { id, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)
    }

    /// Get aggregate session statistics.
    pub async fn get_stats(&self) -> Result<SessionStats, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::GetStats { resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)
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
}

/// Metadata stored for each active torrent.
struct TorrentEntry {
    /// Channel to send messages to the torrent task
    tx: mpsc::Sender<TorrentMessage>,
    /// Handle to the torrent task
    handle: JoinHandle<Result<(), TorrentError>>,
    /// Display name of the torrent
    name: String,
    /// Total size in bytes (0 if metadata not yet fetched)
    size_bytes: u64,
}

/// Internal session manager that runs as a background task.
struct SessionManager {
    config: SessionConfig,
    rx: mpsc::UnboundedReceiver<SessionCommand>,
    sessions: Arc<RwLock<HashMap<InfoHash, TorrentEntry>>>,
    dht_enabled: bool,
}

impl SessionManager {
    pub fn new(config: SessionConfig, rx: mpsc::UnboundedReceiver<SessionCommand>) -> Self {
        let dht_enabled = config.enable_dht;
        Self {
            config,
            rx,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            dht_enabled,
        }
    }

    /// Main entry point - runs the session manager loop.
    pub async fn start(mut self) {
        let tracker = Arc::new(TrackerHandler::new(*CLIENT_ID));
        let storage = Arc::new(Storage::new());

        // TODO: Boostrap this in a tokio task to avoid blocking
        // Initialize and bootstrap DHT if enabled
        let dht: Option<Arc<DhtHandler>> = if self.dht_enabled {
            let config =
                DhtConfig::with_default_persistence(self.config.port).unwrap_or_else(|e| {
                    tracing::warn!(
                        "Failed to create DHT config with persistence: {}, using defaults",
                        e
                    );
                    DhtConfig {
                        port: self.config.port,
                        ..Default::default()
                    }
                });

            match DhtHandler::with_config(config).await {
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

        let (shutdown_tx, shutdown_rx) = watch::channel(());

        self.spawn_tcp_listener();

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                SessionCommand::AddTorrent { info, resp } => {
                    let result = self.handle_add_torrent(
                        info,
                        &tracker,
                        dht.as_ref(),
                        &storage,
                        &shutdown_rx,
                    );
                    let _ = resp.send(result);
                }
                SessionCommand::AddMagnet { magnet, resp } => {
                    let result = self.handle_add_magnet(
                        magnet,
                        &tracker,
                        dht.as_ref(),
                        &storage,
                        &shutdown_rx,
                    );
                    let _ = resp.send(result);
                }
                SessionCommand::RemoveTorrent { id, resp } => {
                    let result = self.handle_remove_torrent(id).await;
                    let _ = resp.send(result);
                }
                SessionCommand::ListTorrents { resp } => {
                    let list = self.handle_list_torrents();
                    let _ = resp.send(list);
                }
                SessionCommand::GetTorrent { id, resp } => {
                    let info = self.handle_get_torrent(id);
                    let _ = resp.send(info);
                }
                SessionCommand::GetStats { resp } => {
                    let stats = self.handle_get_stats(dht.as_ref());
                    let _ = resp.send(stats);
                }
                SessionCommand::Shutdown { resp } => {
                    let result = self.handle_shutdown(&shutdown_tx, dht.as_ref()).await;
                    let _ = resp.send(result);
                    break;
                }
            }
        }
    }

    fn handle_add_torrent(
        &self,
        metainfo: TorrentInfo,
        tracker: &Arc<TrackerHandler>,
        dht: Option<&Arc<DhtHandler>>,
        storage: &Arc<Storage>,
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

        let (torrent, tx) = Torrent::from_torrent_info(
            metainfo,
            tracker.clone(),
            dht.cloned(),
            storage.clone(),
            shutdown_rx.clone(),
        );

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            name,
            size_bytes,
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

        Ok(info_hash)
    }

    fn handle_add_magnet(
        &self,
        magnet: Magnet,
        tracker: &Arc<TrackerHandler>,
        dht: Option<&Arc<DhtHandler>>,
        storage: &Arc<Storage>,
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

        let (torrent, tx) = Torrent::from_magnet(
            magnet,
            tracker.clone(),
            dht.cloned(),
            storage.clone(),
            shutdown_rx.clone(),
        );

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            name,
            size_bytes: 0, // Unknown until metadata is fetched
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

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
                Ok(())
            }
            None => Err(SessionError::TorrentNotFound(id)),
        }
    }

    fn handle_list_torrents(&self) -> Vec<TorrentSummary> {
        let sessions = self.sessions.read().unwrap();
        sessions
            .iter()
            .map(|(id, entry)| {
                // TODO: Query actual stats from torrent when Phase 3 is implemented
                TorrentSummary {
                    id: *id,
                    name: entry.name.clone(),
                    state: TorrentState::Downloading, // Placeholder
                    progress: 0.0,                    // Placeholder
                    download_rate: 0,                 // Placeholder
                    upload_rate: 0,                   // Placeholder
                    peers_connected: 0,               // Placeholder
                    size_bytes: entry.size_bytes,
                    downloaded_bytes: 0, // Placeholder
                }
            })
            .collect()
    }

    fn handle_get_torrent(&self, id: TorrentId) -> Option<TorrentSummary> {
        let sessions = self.sessions.read().unwrap();
        sessions.get(&id).map(|entry| {
            // TODO: Query actual stats from torrent when Phase 3 is implemented
            TorrentSummary {
                id,
                name: entry.name.clone(),
                state: TorrentState::Downloading, // Placeholder
                progress: 0.0,                    // Placeholder
                download_rate: 0,                 // Placeholder
                upload_rate: 0,                   // Placeholder
                peers_connected: 0,               // Placeholder
                size_bytes: entry.size_bytes,
                downloaded_bytes: 0, // Placeholder
            }
        })
    }

    fn handle_get_stats(&self, dht: Option<&Arc<DhtHandler>>) -> SessionStats {
        let torrent_count = self.sessions.read().unwrap().len();

        // TODO: Aggregate actual stats from torrents when Phase 3 is implemented
        SessionStats {
            torrents_downloading: torrent_count,
            torrents_seeding: 0,
            total_download_rate: 0,
            total_upload_rate: 0,
            dht_nodes: dht.as_ref().map(|_| 0), // TODO: Get actual node count
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
        tracing::info!("Starting connection listener on port {}", self.config.port);
        let port = self.config.port;
        let sessions = self.sessions.clone();

        tokio::spawn(async move {
            let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
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
                            })
                            .await;
                    }
                });
            }
        });
    }
}
