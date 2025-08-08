use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use error::TrackerError;
use http::{AnnounceParams, HttpTrackerClient};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
};
use udp::UdpTrackerClient;
// use udp::AnnounceRequest;

use crate::{
    torrent::metainfo::TorrentInfo,
    types::{InfoHash, PeerID},
};

pub mod error;
mod http;
mod udp;

use url::Url;

#[derive(Debug)]
pub struct TrackerResponse {
    pub peers: Vec<SocketAddr>,
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

#[derive(Debug)]
pub enum Actions {
    Connect,
    Announce,
    Scrape,
    Error,
}

impl From<&Actions> for u32 {
    fn from(action: &Actions) -> Self {
        match action {
            Actions::Connect => 0,
            Actions::Announce => 1,
            Actions::Scrape => 2,
            Actions::Error => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Events {
    None,
    Completed,
    Started,
    Stopped,
}

impl From<&Events> for u32 {
    fn from(events: &Events) -> Self {
        match events {
            Events::None => 0,
            Events::Completed => 1,
            Events::Started => 2,
            Events::Stopped => 3,
        }
    }
}

impl Events {
    pub fn to_string(&self) -> Option<String> {
        match self {
            Events::None => None,
            Events::Started => Some("started".to_string()),
            Events::Stopped => Some("stopped".to_string()),
            Events::Completed => Some("completed".to_string()),
        }
    }
}

pub struct TrackerManager {
    rx: mpsc::Receiver<TrackerMessage>,
    udp_sockets: HashMap<SocketAddr, Arc<UdpSocket>>,
    http_client: reqwest::Client,
    tracker_clients: HashMap<InfoHash, Arc<dyn TrackerClient>>,
    client_id: PeerID,
}

pub trait TrackerClient: Send + Sync {}

impl TrackerManager {
    pub fn new(rx: mpsc::Receiver<TrackerMessage>, client_id: PeerID) -> Self {
        Self {
            rx,
            udp_sockets: HashMap::new(),
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("Failed to create HTTP client"),
            tracker_clients: HashMap::new(),
            client_id,
        }
    }

    pub async fn start(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                TrackerMessage::Announce {
                    torrent,
                    response_tx,
                } => {
                    let result = self.handle_announce(torrent).await;
                    let _ = response_tx.send(result);
                }
                _ => unimplemented!(),
            }
        }
    }

    async fn handle_announce(
        &self,
        torrent: Arc<TorrentInfo>,
    ) -> Result<TrackerResponse, TrackerError> {
        let trackers = torrent.all_trackers();

        for tracker_url in trackers.iter() {
            dbg!(tracker_url);
            match self.try_announce(tracker_url, torrent.clone()).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    tracing::warn!("Tracker {} failed: {}", tracker_url, e);
                    continue;
                }
            }
        }
        Err(TrackerError::InvalidResponse(
            "All trackers failed".to_string(),
        ))
    }

    async fn try_announce(
        &self,
        tracker_url: &str,
        torrent: Arc<TorrentInfo>,
    ) -> Result<TrackerResponse, TrackerError> {
        let url = Url::parse(tracker_url)?;
        let params = AnnounceParams {
            info_hash: torrent.info_hash,
            peer_id: self.client_id,
            port: 6881, // TODO: Make configurable
            uploaded: 0,
            downloaded: 0,
            left: torrent.total_size(),
            event: Events::Started,
            compact: true,
        };
        match url.scheme() {
            "http" | "https" => {
                let client = HttpTrackerClient::new(tracker_url, &self.http_client)?;
                client.announce(&params).await
            }
            "udp" => {
                dbg!("GOing full UDP");
                let mut client = UdpTrackerClient::new(tracker_url).await?;
                client.announce(&params).await
            }
            scheme => todo!(),
        }
    }
}

pub enum TrackerMessage {
    Announce {
        torrent: Arc<TorrentInfo>,
        // params: AnnounceParams,
        response_tx: oneshot::Sender<Result<TrackerResponse, TrackerError>>,
    },
    Scrape {
        torrent: Arc<TorrentInfo>,
        // response_tx: oneshot::Sender<Result<ScrapeResponse, TrackerError>>,
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
        let tracker_actor = TrackerManager::new(tracker_rx, client_id);
        tokio::spawn(tracker_actor.start());
        Self {
            tracker_tx,
            // tracker_task,
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use rand::Rng;
    use tokio::sync::{mpsc, oneshot};

    use crate::{
        client::PORT,
        torrent::metainfo::parse_torrent_from_file,
        tracker::{
            Events, TrackerHandler, TrackerManager, TrackerMessage,
            http::{AnnounceParams, HttpTrackerClient},
            udp::UdpTrackerClient,
        },
        types::PeerID,
    };

    fn generate_peer_id() -> PeerID {
        let mut peer_id = [0u8; 20];
        peer_id[0..3].copy_from_slice(b"-RS"); // Client identifier
        rand::rng().fill(&mut peer_id[3..]); // Random bytes
        peer_id.into()
    }

    #[tokio::test]
    async fn http_test_with_real_torrent() {
        // This test requires internet access and a real torrent file

        let file = "../sample_torrents/sample.torrent";
        let torrent = parse_torrent_from_file(file).expect("Failed to parse torrent");
        dbg!(hex::encode(torrent.info_hash));
        let torrent = Arc::new(torrent);
        let peer_id = generate_peer_id();

        // Find first HTTP tracker
        let http_tracker = torrent
            .all_trackers()
            .into_iter()
            .find(|url| url.starts_with("http"))
            .expect("No HTTP trackers found");

        // Test HTTP tracker client directly
        let client = reqwest::Client::new();
        let http_client = HttpTrackerClient::new(&http_tracker, &client)
            // .await
            .unwrap();

        let params = AnnounceParams {
            info_hash: torrent.info_hash,
            peer_id,
            port: PORT,
            uploaded: 0,
            downloaded: 0,
            left: torrent.total_size(),
            event: Events::Started,
            compact: true,
        };

        let result = http_client.announce(&params).await;
        assert!(result.is_ok(), "Announce failed: {:?}", result.err());

        // safety: This is alredy checked above
        let r = result.unwrap();
        println!("{:?}", r);

        // Test full TrackerManager flow
        let (tx, rx) = mpsc::channel(100);
        let manager = TrackerManager::new(rx, peer_id);
        tokio::spawn(manager.start());

        let (response_tx, response_rx) = oneshot::channel();
        tx.send(TrackerMessage::Announce {
            torrent: torrent.clone(),
            response_tx,
        })
        .await
        .unwrap();

        let response = response_rx.await.unwrap();
        assert!(
            response.is_ok(),
            "TrackerManager announce failed: {:?}",
            response.err()
        );
    }
}
