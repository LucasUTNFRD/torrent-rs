use std::{sync::Arc, time::Duration};

use bittorrent_core::{
    metainfo::TorrentInfo,
    types::{InfoHash, PeerID},
};

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

// TODO: restructure crate

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
            tracing::debug!("Announcing to tracker: {}", tracker_url);
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

        let client = clients.select_client_for_scheme(url.scheme()).unwrap();

        // client.announce(&params, url).await
        match timeout(Duration::from_secs(10), client.announce(&params, url)).await {
            Ok(res) => res,                       // result from announce
            Err(_) => Err(TrackerError::Timeout), // you'd need to add this variant
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct ClientState {
    downloaded: i64,
    left: i64,
    uploaded: i64,
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

    pub async fn announce(
        &self,
        torrent: Arc<TorrentInfo>,
    ) -> Result<TrackerResponse, TrackerError> {
        let (otx, orx) = oneshot::channel();
        let _ = self
            .tracker_tx
            .send(TrackerMessage::Announce {
                torrent,
                response_tx: otx,
            })
            .await;

        // FIX: Error mapping
        orx.await.map_err(|e| TrackerError::Timeout)?
    }
}

#[cfg(all(test, feature = "real_trackers"))]
mod test {
    use std::sync::Arc;

    use bittorent_core::{client::PORT, torrent::metainfo::parse_torrent_from_file, types::PeerID};
    use futures::future::join_all;
    use rand::Rng;
    use tokio::sync::oneshot;

    use crate::{
        TrackerHandler, UdpTrackerClient,
        client::{TrackerClient, TrackerMessage},
        http::HttpTrackerClient,
        types::{AnnounceParams, Events},
    };

    //
    fn generate_peer_id() -> PeerID {
        let mut peer_id = [0u8; 20];
        peer_id[0..3].copy_from_slice(b"-RS"); // Client identifier
        rand::rng().fill(&mut peer_id[3..]); // Random bytes
        peer_id.into()
    }
    //
    #[tokio::test]
    async fn http_test_with_real_torrent() {
        // This test requires internet access and a real torrent file

        let file = "../sample_torrents/debian-12.10.0-amd64-netinst.iso.torrent";

        let torrent = parse_torrent_from_file(file).expect("Failed to parse torrent");
        dbg!(hex::encode(torrent.info_hash));
        dbg!(torrent.info_hash);
        let torrent = Arc::new(torrent);
        let peer_id = generate_peer_id();

        // Find first HTTP tracker
        let tracker = torrent
            .all_trackers()
            .into_iter()
            .find(|url| url.starts_with("http"))
            .expect("No HTTP trackers found");

        // Test HTTP tracker client directly
        let client = HttpTrackerClient::new().unwrap();

        let params = AnnounceParams {
            info_hash: torrent.info_hash,
            peer_id,
            port: PORT,
            uploaded: 0,
            downloaded: 0,
            left: torrent.total_size(),
            event: Events::Started,
            // compact: true,
        };

        let tracker_url = url::Url::parse(&tracker).unwrap();
        let result = client.announce(&params, tracker_url).await;
        assert!(result.is_ok(), "Announce failed: {:?}", result.err());

        // safety: This is alredy checked above
        let r = result.unwrap();
        println!("result {r:?}");
    }

    #[tokio::test]
    async fn test_two_torrents_announcing_via_same_handler() {
        // Parse two real torrents (with working trackers)
        let file1 = "../sample_torrents/big-buck-bunny.torrent";
        let file2 = "../sample_torrents/debian-12.10.0-amd64-netinst.iso.torrent";

        let torrent1 = parse_torrent_from_file(file1).expect("Failed to parse torrent1");
        let torrent2 = parse_torrent_from_file(file2).expect("Failed to parse torrent2");
        let torrent1 = Arc::new(torrent1);
        let torrent2 = Arc::new(torrent2);

        let client_id = generate_peer_id();

        let tracker_handler = TrackerHandler::new(client_id);

        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();

        // Send announce message for torrent1
        tracker_handler
            .tracker_tx
            .send(TrackerMessage::Announce {
                torrent: torrent1.clone(),
                response_tx: tx1,
            })
            .await
            .expect("failed to send announce for torrent1");

        // Send announce message for torrent2
        tracker_handler
            .tracker_tx
            .send(TrackerMessage::Announce {
                torrent: torrent2.clone(),
                response_tx: tx2,
            })
            .await
            .expect("failed to send announce for torrent2");

        // Await both responses
        let (res1, res2) = tokio::join!(rx1, rx2);

        let res1 = res1.expect("announce1 channel dropped");
        let res2 = res2.expect("announce2 channel dropped");

        // Assert both succeeded
        assert!(res1.is_ok(), "torrent1 announce failed: {:?}", res1.err());
        assert!(res2.is_ok(), "torrent2 announce failed: {:?}", res2.err());

        // Assert both have peers
        let peers1 = res1.unwrap().peers;
        let peers2 = res2.unwrap().peers;

        assert!(!peers1.is_empty(), "torrent1 returned no peers");
        assert!(!peers2.is_empty(), "torrent2 returned no peers");

        println!("Torrent1 peers: {}", peers1.len());
        println!("Torrent2 peers: {}", peers2.len());
    }

    #[tokio::test]
    async fn udp_test_with_real_torrent() {
        // This test requires internet access and a real torrent file

        let file = "../sample_torrents/linuxmint-21.2-mate-64bit.iso.torrent";

        let torrent = parse_torrent_from_file(file).expect("Failed to parse torrent");
        dbg!(hex::encode(torrent.info_hash));
        let torrent = Arc::new(torrent);
        let peer_id = generate_peer_id();

        // Find first udp tracker
        let trackers = torrent.all_trackers();

        // Test udp tracker client directly
        let client = UdpTrackerClient::new()
            .await
            .expect("failed to start udp tracker client");
        let client = Arc::new(client);

        let params = AnnounceParams {
            info_hash: torrent.info_hash,
            peer_id,
            port: PORT,
            uploaded: 0,
            downloaded: 0,
            left: torrent.total_size(),
            event: Events::Started,
            // compact: true,
        };

        // Create tasks for parallel execution
        let tasks: Vec<_> = trackers
            .into_iter()
            .map(|tracker| {
                let client = client.clone();
                let params = params.clone();

                tokio::spawn(async move {
                    let tracker_url = match url::Url::parse(&tracker) {
                        Ok(url) => url,
                        Err(e) => {
                            return (tracker, Err(format!("URL parse error: {:?}", e)));
                        }
                    };

                    match client.announce(&params, tracker_url).await {
                        Ok(response) => {
                            println!("{response:?}");
                            (tracker, Ok(response))
                        }
                        Err(e) => (tracker, Err(format!("{:?}", e))),
                    }
                })
            })
            .collect();

        // Wait for all tasks to complete
        let results = join_all(tasks).await;

        // Process results
        let mut successful = 0;
        let mut failed = 0;

        for task_result in results {
            match task_result {
                Ok((tracker, announce_result)) => match announce_result {
                    Ok(_) => {
                        successful += 1;
                    }
                    Err(e) => {
                        failed += 1;
                    }
                },
                Err(join_error) => {
                    failed += 1;
                }
            }
        }

        println!("successful {successful} ; Failed {failed}");
    }
}
