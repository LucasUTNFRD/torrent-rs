use std::{sync::Arc, time::Duration};

use bittorrent_common::types::{InfoHash, PeerID};

use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use url::Url;

use crate::{
    TrackerError,
    http::HttpTrackerClient,
    types::{AnnounceParams, Events, TrackerResponse},
    udp::UdpTrackerClient,
};

// =============================================================================
// Core Traits and Types
// =============================================================================
pub struct TrackerManager {
    rx: mpsc::Receiver<TrackerMessage>,
    client_id: PeerID,
    tracker_clients: Arc<Clients>,
}

#[derive(Clone)]
struct Clients {
    udp: Arc<UdpTrackerClient>,
    http: Arc<HttpTrackerClient>,
}
#[async_trait::async_trait]
pub trait TrackerClient: Send + Sync {
    async fn announce(
        &self,
        params: &AnnounceParams,
        tracker: Url,
    ) -> Result<TrackerResponse, TrackerError>;
}

impl Clients {
    pub async fn new() -> Self {
        let udp = UdpTrackerClient::new()
            .await
            .expect("failure to start udp tracker client");
        let udp = Arc::new(udp);

        let http = HttpTrackerClient::new().expect("failure to start http tracker client");
        let http = Arc::new(http);

        Self { udp, http }
    }

    pub fn select_client_for_scheme(
        &self,
        scheme: &str,
    ) -> Result<Arc<dyn TrackerClient>, TrackerError> {
        match scheme {
            "udp" => Ok(self.udp.clone()),
            "http" | "https" => Ok(self.http.clone()),
            _ => Err(TrackerError::InvalidScheme(scheme.to_string())),
        }
    }
}

impl TrackerManager {
    pub async fn new(rx: mpsc::Receiver<TrackerMessage>, client_id: PeerID) -> Self {
        Self {
            rx,
            tracker_clients: Arc::new(Clients::new().await),
            client_id,
        }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                TrackerMessage::Announce {
                    info_hash,
                    tracker,
                    client_state,
                    response_tx,
                } => {
                    let client_id = self.client_id;
                    let clients = self.tracker_clients.clone();
                    tokio::spawn(async move {
                        let announce_result = Self::try_announce(
                            client_id,
                            client_state,
                            &tracker,
                            info_hash,
                            clients.clone(),
                        )
                        .await;

                        let _ = response_tx.send(announce_result);
                    });
                }

                _ => unimplemented!(),
            }
        }
    }

    async fn try_announce(
        // &self
        client_id: PeerID,
        client_state: ClientState,
        tracker_url: &str,
        torrent: InfoHash,
        clients: Arc<Clients>,
    ) -> Result<TrackerResponse, TrackerError> {
        let url = Url::parse(tracker_url)?;
        let params = AnnounceParams {
            info_hash: torrent,
            peer_id: client_id,
            port: 6881, // TODO: Make configurable
            uploaded: client_state.uploaded,
            downloaded: client_state.downloaded,
            left: client_state.left,
            event: client_state.event,
        };

        let client = clients.select_client_for_scheme(url.scheme())?;

        // client.announce(&params, url).await
        match timeout(Duration::from_secs(10), client.announce(&params, url)).await {
            Ok(res) => res,
            Err(_) => Err(TrackerError::Timeout),
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct ClientState {
    downloaded: i64,
    left: i64,
    uploaded: i64,
    #[allow(dead_code)]
    event: Events,
}

impl ClientState {
    pub fn new(
        bytes_downloaded: i64,
        bytes_left: i64,
        bytes_uploaded: i64,
        event: Events,
    ) -> ClientState {
        ClientState {
            downloaded: bytes_downloaded,
            left: bytes_left,
            uploaded: bytes_uploaded,
            event,
        }
    }
}

// the idea is to use client state to avoid sharing a reference to torrent info, and only send
// client state + tracker url + response_tx to

pub enum TrackerMessage {
    /// Legacy behaviours - given a list of trackers the first to resolve - send to the oneshot sender
    Announce {
        info_hash: InfoHash,
        client_state: ClientState,
        tracker: String,
        response_tx: oneshot::Sender<Result<TrackerResponse, TrackerError>>,
    },
    Stop {
        info_hash: InfoHash,
        // response_tx: oneshot::Sender<Result<(), TrackerError>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct TrackerHandler {
    tracker_tx: mpsc::Sender<TrackerMessage>,
}

impl TrackerHandler {
    pub fn new(client_id: PeerID) -> Self {
        let (tracker_tx, tracker_rx) = mpsc::channel(100);
        tokio::spawn(async move {
            let tracker_actor = TrackerManager::new(tracker_rx, client_id).await;
            tracker_actor.run().await
        });
        Self { tracker_tx }
    }

    ///  Single announce (legacy) - returns first successful response
    pub async fn announce(
        &self,
        info_hash: InfoHash,
        tracker: String,
        client_state: ClientState,
    ) -> Result<TrackerResponse, TrackerError> {
        let (otx, orx) = oneshot::channel();
        let _ = self
            .tracker_tx
            .send(TrackerMessage::Announce {
                info_hash,
                tracker,
                client_state,
                response_tx: otx,
            })
            .await;

        orx.await.map_err(|_| TrackerError::UnableToConnect)?
    }

    pub async fn scrape(&self) {}
}
