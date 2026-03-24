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
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{
        broadcast,
        mpsc::{self, UnboundedSender},
        oneshot, watch,
    },
    task::{self, JoinHandle},
};

use mainline_dht::{DhtConfig, DhtHandler};
use tracker_client::TrackerHandler;

use crate::{
    SessionConfig, TorrentProgress,
    events::SessionEvent,
    metrics::counters::{self},
    net::TcpListener,
    protocol::peer_wire::Handshake,
    storage::{DiskStorage, StorageBackend},
    torrent::{Torrent, TorrentContext, TorrentError, TorrentMessage, TorrentSource},
    types::TorrentId,
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
    pub event_bus: crate::events::EventBus,
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

    pub fn with_storage(mut self, storage: Arc<dyn StorageBackend>) -> Self {
        self.storage = Some(storage);
        self
    }

    pub fn build(self) -> Session {
        let (tx, rx) = mpsc::unbounded_channel();
        let event_bus = crate::events::EventBus::new();

        let storage = self.storage.unwrap_or_else(|| Arc::new(DiskStorage::new()));

        let manager = SessionManager::new(self.config, rx, storage, event_bus.clone());

        let handle = tokio::task::spawn(async move { manager.start().await });

        Session {
            handle,
            event_bus,
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
    /// Seed a torrent without verifying content on disk.
    /// Used for simulation testing where storage is mock/in-memory.
    SeedTorrentUnchecked {
        info: TorrentInfo,
        resp: oneshot::Sender<Result<TorrentId, SessionError>>,
    },
    GetMetrics {
        id: TorrentId,
        resp: oneshot::Sender<Result<watch::Receiver<TorrentProgress>, SessionError>>,
    },
    Shutdown {
        resp: oneshot::Sender<Result<(), SessionError>>,
    },
    ConnectPeer {
        id: TorrentId,
        addr: std::net::SocketAddr,
        resp: oneshot::Sender<Result<(), SessionError>>,
    },
    GetTorrentDetail {
        id: TorrentId,
        resp: oneshot::Sender<Result<TorrentDetail, SessionError>>,
    },
    GetPeerSnapshots {
        id: TorrentId,
        resp: oneshot::Sender<Result<Vec<PeerSnapshot>, SessionError>>,
    },
    GetTrackerStatuses {
        id: TorrentId,
        resp: oneshot::Sender<Result<Vec<TrackerStatus>, SessionError>>,
    },
}

// Re-export detail types from detail module
pub use crate::detail::{PeerSnapshot, TorrentDetail, TrackerStatus};

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

    #[error("Metadata not yet available for torrent: {0}. Waiting for peer connection...")]
    MetadataPending(TorrentId),
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

    /// Add a torrent from a pre-built `TorrentInfo` struct.
    ///
    /// This is primarily useful for simulation testing where torrent
    /// metadata is generated programmatically.
    pub async fn add_torrent_info(&self, info: TorrentInfo) -> Result<TorrentId, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::AddTorrent { info, resp: tx })
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

    /// Seed a torrent without verifying content on disk.
    ///
    /// This is used for simulation testing where storage is mock/in-memory
    /// and no real files exist on disk.
    pub async fn seed_torrent_unchecked(
        &self,
        info: TorrentInfo,
    ) -> Result<TorrentId, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::SeedTorrentUnchecked { info, resp: tx })
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

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_bus.subscribe_session()
    }

    pub fn subscribe_torrent_events(&self) -> broadcast::Receiver<crate::events::TorrentEvent> {
        self.event_bus.subscribe_torrent()
    }

    pub fn subscribe_peer_events(&self) -> broadcast::Receiver<crate::events::PeerEvent> {
        self.event_bus.subscribe_peer()
    }

    /// Subscribe to metrics for a specific torrent.
    pub async fn subscribe_torrent(
        &self,
        id: TorrentId,
    ) -> Result<watch::Receiver<TorrentProgress>, SessionError> {
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

    /// Instruct a torrent to connect to a peer at the given address.
    ///
    /// This bypasses tracker/DHT discovery and is primarily used for
    /// simulation testing where peers are known ahead of time.
    pub async fn connect_peer(
        &self,
        id: TorrentId,
        addr: std::net::SocketAddr,
    ) -> Result<(), SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::ConnectPeer { id, addr, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    pub fn suscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.subscribe()
    }

    pub async fn get_torrent_meta(&self, id: TorrentId) -> Result<TorrentDetail, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::GetTorrentDetail { id, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    /// Get peer connection snapshots for a torrent
    pub async fn get_peers(&self, id: TorrentId) -> Result<Vec<PeerSnapshot>, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::GetPeerSnapshots { id, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }

    /// Get tracker statuses for a torrent
    pub async fn get_trackers(&self, id: TorrentId) -> Result<Vec<TrackerStatus>, SessionError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(SessionCommand::GetTrackerStatuses { id, resp: tx })
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)?
    }
}

/// Metadata stored for each active torrent.
#[allow(dead_code)]
struct TorrentEntry {
    /// Channel to send messages to the torrent task
    tx: mpsc::Sender<TorrentMessage>,
    /// Handle to the torrent task
    handle: JoinHandle<Result<(), TorrentError>>,
    /// Metrics receiver for the torrent
    progress_rx: watch::Receiver<TorrentProgress>,
    /// Display name of the torrent
    name: String,
    /// Total size in bytes (0 if metadata not yet fetched)
    size_bytes: u64,
    /// Torrent metadata (None for magnet links until metadata is fetched)
    metainfo: Option<Arc<TorrentInfo>>,
}

/// Internal session manager that runs as a background task.
struct SessionManager {
    config: SessionConfig,
    peer_id: PeerID,
    event_bus: crate::events::EventBus,
    rx: mpsc::UnboundedReceiver<SessionCommand>,
    sessions: Arc<RwLock<HashMap<InfoHash, TorrentEntry>>>,
    storage: Arc<dyn StorageBackend>,
}

impl SessionManager {
    pub fn new(
        config: SessionConfig,
        rx: mpsc::UnboundedReceiver<SessionCommand>,
        storage: Arc<dyn StorageBackend>,
        event_bus: crate::events::EventBus,
    ) -> Self {
        let peer_id = config.peer_id.unwrap_or(*CLIENT_ID);
        Self {
            config,
            peer_id,
            event_bus,
            rx,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            storage,
        }
    }

    /// Main entry point - runs the session manager loop.
    pub async fn start(mut self) {
        // TrackerHandler eagerly binds a UDP socket, which turmoil doesn't support.
        // In sim mode, create a no-op handler (announce calls will return channel errors).
        #[cfg(not(feature = "sim"))]
        let tracker = Arc::new(TrackerHandler::new(*CLIENT_ID));
        #[cfg(feature = "sim")]
        let tracker = Arc::new(TrackerHandler::new_noop());

        let dht = self.initialize_dht().await;

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
                    let dir = content_dir.clone();
                    let info_clone = info.clone();

                    let is_valid = task::spawn_blocking(move || {
                        verify_content(&dir, &info_clone).expect("Failed to verify")
                    })
                    .await
                    .unwrap_or(false);

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
                SessionCommand::SeedTorrentUnchecked { info, resp } => {
                    // Skip verify_content — used for simulation with mock storage
                    let result = self
                        .handle_seed_torrent(
                            info,
                            PathBuf::new(),
                            &tracker,
                            dht.as_ref(),
                            &shutdown_rx,
                        )
                        .await;
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
                            .map(|entry| entry.progress_rx.clone())
                            .ok_or(SessionError::TorrentNotFound(id))
                    };
                    let _ = resp.send(result);
                }

                SessionCommand::Shutdown { resp } => {
                    let result = self.handle_shutdown(&shutdown_tx, dht.as_ref()).await;
                    let _ = resp.send(result);
                    break;
                }
                SessionCommand::ConnectPeer { id, addr, resp } => {
                    let result = {
                        let sessions = self.sessions.read().unwrap();
                        match sessions.get(&id) {
                            Some(entry) => {
                                let _ = entry.tx.try_send(TorrentMessage::ConnectPeer { addr });
                                Ok(())
                            }
                            None => Err(SessionError::TorrentNotFound(id)),
                        }
                    };
                    let _ = resp.send(result);
                }
                SessionCommand::GetTorrentDetail { id, resp } => {
                    let result = self
                        .query_torrent(id, |tx| TorrentMessage::GetTorrentDetail { resp: tx })
                        .await;
                    let _ = resp.send(result);
                }
                SessionCommand::GetPeerSnapshots { id, resp } => {
                    let result = self
                        .query_torrent(id, |tx| TorrentMessage::GetPeerSnapshots { resp: tx })
                        .await;
                    let _ = resp.send(result);
                }
                SessionCommand::GetTrackerStatuses { id, resp } => {
                    let result = self
                        .query_torrent(id, |tx| TorrentMessage::GetTrackerStatuses { resp: tx })
                        .await;
                    let _ = resp.send(result);
                }
            }
        }
    }

    /// Initialize DHT if enabled. Handles graceful degradation on failure.
    async fn initialize_dht(&self) -> Option<Arc<DhtHandler>> {
        if !self.config.enable_dht {
            tracing::info!("DHT disabled by configuration");
            return None;
        }

        let config_dir = &self.config.config_dir;
        let dht_config = DhtConfig {
            id_file_path: Some(config_dir.join("node.id")),
            state_file_path: Some(config_dir.join("dht_state.dat")),
            port: self.config.listen_addr.port(),
        };

        let dht = DhtHandler::with_config(dht_config)
            .await
            .inspect_err(|e| {
                tracing::warn!("Failed to create DHT node: {e}, continuing without DHT");
            })
            .ok()?;

        tracing::info!("DHT node created, bootstrapping...");

        if let Err(e) = dht.bootstrap().await {
            tracing::warn!("DHT bootstrap failed: {e}, continuing without DHT");
            return None;
        }

        tracing::info!(
            "DHT bootstrapped successfully with node ID: {:?}",
            dht.node_id()
        );
        Some(Arc::new(dht))
    }

    /// Helper to forward a oneshot-bearing message to a torrent actor and await the reply.
    /// Handles the common pattern of: lookup entry, clone channel, send message, await response.
    async fn query_torrent<T>(
        &self,
        id: TorrentId,
        make_msg: impl FnOnce(oneshot::Sender<T>) -> TorrentMessage,
    ) -> Result<T, SessionError> {
        let (tx, rx) = oneshot::channel();
        let torrent_tx = {
            let sessions = self.sessions.read().unwrap();
            sessions
                .get(&id)
                .map(|e| e.tx.clone())
                .ok_or(SessionError::TorrentNotFound(id))?
        };
        torrent_tx
            .try_send(make_msg(tx))
            .map_err(|_| SessionError::SessionClosed)?;
        rx.await.map_err(|_| SessionError::SessionClosed)
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

        let ctx = TorrentContext {
            peer_id: self.peer_id,
            tracker_client: tracker.clone(),
            dht_client: dht.cloned(),
            storage: self.storage.clone(),
            shutdown_rx: shutdown_rx.clone(),
            torrents_dir: self.config.torrents_dir.clone(),
            event_bus: self.event_bus.clone(),
            unchoke_slots: self.config.unchoke_slots_limit as usize,
        };

        let (torrent, tx, progress_rx) = Torrent::new(
            ctx,
            TorrentSource::Seed {
                torrent_info: metainfo.clone(),
                content_dir,
            },
        );

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            progress_rx: progress_rx.clone(),
            name: name.clone(),
            size_bytes,
            metainfo: Some(Arc::new(metainfo)),
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

        let _ = self
            .event_bus
            .session_tx
            .send(SessionEvent::TorrentAdded(info_hash));

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

        let ctx = TorrentContext {
            peer_id: self.peer_id,
            tracker_client: tracker.clone(),
            dht_client: dht.cloned(),
            storage: self.storage.clone(),
            shutdown_rx: shutdown_rx.clone(),
            torrents_dir: self.config.torrents_dir.clone(),
            event_bus: self.event_bus.clone(),
            unchoke_slots: self.config.unchoke_slots_limit as usize,
        };

        let (torrent, tx, progress_rx) =
            Torrent::new(ctx, TorrentSource::Torrent(metainfo.clone()));

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            progress_rx: progress_rx.clone(),
            name: name.clone(),
            size_bytes,
            metainfo: Some(Arc::new(metainfo)),
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

        let _ = self
            .event_bus
            .session_tx
            .send(SessionEvent::TorrentAdded(info_hash));

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

        let ctx = TorrentContext {
            peer_id: self.peer_id,
            tracker_client: tracker.clone(),
            dht_client: dht.cloned(),
            storage: self.storage.clone(),
            shutdown_rx: shutdown_rx.clone(),
            torrents_dir: self.config.torrents_dir.clone(),
            event_bus: self.event_bus.clone(),
            unchoke_slots: self.config.unchoke_slots_limit as usize,
        };

        let (torrent, tx, progress_rx) = Torrent::new(ctx, TorrentSource::Magnet(magnet));

        let handle = tokio::spawn(async move { torrent.start_session().await });

        let entry = TorrentEntry {
            tx,
            handle,
            progress_rx,
            name,
            size_bytes: 0,  // Unknown until metadata is fetched
            metainfo: None, // Will be populated when metadata is fetched
        };

        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(info_hash, entry);
        }

        let _ = self
            .event_bus
            .session_tx
            .send(SessionEvent::TorrentAdded(info_hash));

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
                let _ = self
                    .event_bus
                    .session_tx
                    .send(SessionEvent::TorrentRemoved(id));
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
        let peer_id = self.peer_id;

        tokio::spawn(async move {
            let listener = TcpListener::bind(listen_addr)
                .await
                .expect("failed to bind tcp listener");
            tracing::info!("Listening on {:?}", listener.local_addr());

            while let Ok((mut stream, remote_addr)) = listener.accept().await {
                tracing::debug!("Accepted connection from {:?}", remote_addr);
                counters::incoming_connections();
                let sessions = sessions.clone();
                let peer_id = peer_id;

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

                    if remote_handshake.peer_id == peer_id {
                        tracing::debug!("Rejecting self-connection from {:?}", remote_addr);
                        return;
                    }

                    let handshake = Handshake::new(peer_id, remote_handshake.info_hash);
                    if let Err(e) = stream.write_all(&handshake.to_bytes()).await {
                        tracing::debug!(
                            error = ?e,
                            "Failed to send handshake to {:?}",
                            remote_addr
                        );
                        return;
                    }

                    let supports_ext = remote_handshake.support_extended_message();
                    let dht_enabled = remote_handshake.support_dht();

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
                                dht_enabled,
                            })
                            .await;
                    }
                });
            }
        });
    }
}
