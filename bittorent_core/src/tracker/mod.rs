use std::{net::SocketAddr, sync::Arc};

use error::TrackerError;
use http::HttpTrackerClient;
use tokio::sync::{mpsc, oneshot};
use udp::UdpTrackerClient;

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

#[derive(Debug, Clone)]
pub struct AnnounceParams {
    pub info_hash: InfoHash,
    pub peer_id: PeerID,
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: Events,
    // pub compact: bool,
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
    // cmd_rx
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
    pub fn get_client(&self, scheme: &str) -> Result<Arc<dyn TrackerClient>, TrackerError> {
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
                    torrent,
                    response_tx,
                } => {
                    let client_id = self.client_id;
                    let clients = self.tracker_clients.clone();
                    tokio::spawn(async move {
                        let announce_result =
                            Self::handle_announce(torrent, client_id, clients).await;
                        let _ = response_tx.send(announce_result);
                    });
                }
                _ => unimplemented!(),
            }
        }
    }

    async fn handle_announce(
        torrent: Arc<TorrentInfo>,
        client_id: PeerID,
        clients: Arc<Clients>,
    ) -> Result<TrackerResponse, TrackerError> {
        let trackers = torrent.all_trackers();

        for tracker_url in trackers.iter() {
            dbg!(tracker_url);
            match Self::try_announce(client_id, tracker_url, torrent.clone(), clients.clone()).await
            {
                Ok(response) => return Ok(response),
                Err(e) => {
                    tracing::warn!("Tracker {} failed: {}", tracker_url, e);
                    continue;
                }
            }
        }

        // FIX ME: Use a better Error
        Err(TrackerError::InvalidResponse(
            "All trackers failed".to_string(),
        ))
    }

    async fn try_announce(
        // &self
        client_id: PeerID,
        tracker_url: &str,
        torrent: Arc<TorrentInfo>,
        clients: Arc<Clients>,
    ) -> Result<TrackerResponse, TrackerError> {
        let url = Url::parse(tracker_url)?;
        let params = AnnounceParams {
            info_hash: torrent.info_hash,
            peer_id: client_id,
            port: 6881, // TODO: Make configurable
            uploaded: 0,
            downloaded: 0,
            left: torrent.total_size(),
            event: Events::Started,
            // compact: true,
        };

        let client = clients.get_client(url.scheme()).unwrap();
        client.announce(&params, url).await
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
        tokio::spawn(async move {
            let tracker_actor = TrackerManager::new(tracker_rx, client_id).await;
            tracker_actor.run().await
        });
        Self { tracker_tx }
    }
}

// #[cfg(test)]
// mod test {
//     use std::sync::Arc;
//
//     use rand::Rng;
//     use tokio::sync::{mpsc, oneshot};
//
//     use crate::{
//         client::PORT,
//         torrent::metainfo::parse_torrent_from_file,
//         tracker::{
//             Events, TrackerManager, TrackerMessage,
//             http::{AnnounceParams, HttpTrackerClient},
//         },
//         types::PeerID,
//     };
//
//     fn generate_peer_id() -> PeerID {
//         let mut peer_id = [0u8; 20];
//         peer_id[0..3].copy_from_slice(b"-RS"); // Client identifier
//         rand::rng().fill(&mut peer_id[3..]); // Random bytes
//         peer_id.into()
//     }
//
//     #[tokio::test]
//     async fn http_test_with_real_torrent() {
//         // This test requires internet access and a real torrent file
//
//         let file = "../sample_torrents/sample.torrent";
//         let torrent = parse_torrent_from_file(file).expect("Failed to parse torrent");
//         dbg!(hex::encode(torrent.info_hash));
//         let torrent = Arc::new(torrent);
//         let peer_id = generate_peer_id();
//
//         // Find first HTTP tracker
//         let http_tracker = torrent
//             .all_trackers()
//             .into_iter()
//             .find(|url| url.starts_with("http"))
//             .expect("No HTTP trackers found");
//
//         // Test HTTP tracker client directly
//         let client = reqwest::Client::new();
//         let http_client = HttpTrackerClient::new(&http_tracker, &client)
//             // .await
//             .unwrap();
//
//         let params = AnnounceParams {
//             info_hash: torrent.info_hash,
//             peer_id,
//             port: PORT,
//             uploaded: 0,
//             downloaded: 0,
//             left: torrent.total_size(),
//             event: Events::Started,
//             compact: true,
//         };
//
//         let result = http_client.announce(&params).await;
//         assert!(result.is_ok(), "Announce failed: {:?}", result.err());
//
//         // safety: This is alredy checked above
//         let r = result.unwrap();
//         println!("{:?}", r);
//
//         // Test full TrackerManager flow
//         let (tx, rx) = mpsc::channel(100);
//         let manager = TrackerManager::new(rx, peer_id);
//         tokio::spawn(manager.run());
//
//         let (response_tx, response_rx) = oneshot::channel();
//         tx.send(TrackerMessage::Announce {
//             torrent: torrent.clone(),
//             response_tx,
//         })
//         .await
//         .unwrap();
//
//         let response = response_rx.await.unwrap();
//         assert!(
//             response.is_ok(),
//             "TrackerManager announce failed: {:?}",
//             response.err()
//         );
//     }
// }
