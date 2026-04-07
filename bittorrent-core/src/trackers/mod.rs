use std::net::SocketAddr;

use bencode::BencodeError;
use bittorrent_common::types::{InfoHash, PeerID};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use url::Url;

mod http;
mod udp;

#[derive(Debug, Clone)]
pub enum Events {
    None,
    Completed,
    Started,
    Stopped,
}

impl Events {
    pub fn as_int(&self) -> i32 {
        match self {
            Events::None => 0,
            Events::Completed => 1,
            Events::Started => 2,
            Events::Stopped => 3,
        }
    }

    pub fn as_str(&self) -> Option<&'static str> {
        match self {
            Events::None => None,
            Events::Started => Some("started"),
            Events::Stopped => Some("stopped"),
            Events::Completed => Some("completed"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnnounceData {
    pub info_hash: InfoHash,
    pub peer_id: PeerID,
    pub port: u16,
    pub uploaded: i64,
    pub downloaded: i64,
    pub left: i64,
    pub event: Events,
}

pub struct AnnounceResponse {
    pub peers: Vec<SocketAddr>,
    pub interval: i32,
    pub leechers: i32,
    pub seeders: i32,
}

enum TrackerCmd {
    Announce {
        url: Url,
        data: AnnounceData,
        responder: oneshot::Sender<Result<AnnounceResponse, TrackerError>>,
    },
    Scrape {
        url: Url,
        responder: oneshot::Sender<Result<ScrapeResponse, TrackerError>>,
    },
}

struct ScrapeResponse {}

#[derive(Debug, Error)]
pub enum TrackerError {
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("Invalid tracker response: {0}")]
    InvalidResponse(String),
    #[error("Connection timeout")]
    Timeout,
    #[error("Invalid announce URL scheme: {0}")]
    InvalidScheme(String),
    #[error("Tracker returned error: {0}")]
    TrackerError(String),
    #[error("UDP connection failed")]
    UdpConnectionFailed,
    #[error("Transaction ID mismatch")]
    TransactionMismatch,
    #[error("BencodeError {0}")]
    BencodeError(BencodeError),
    #[error("Invalid string")]
    InvalidString,
    #[error("Invalid Url {0}")]
    InvalidUrl(String),
    #[error("Packet is too short")]
    TooShort,
    #[error("could not establish connection to a tracker")]
    UnableToConnect,
}

#[derive(Clone)]
pub struct Tracker {
    tx: mpsc::Sender<TrackerCmd>,
}

impl Tracker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<TrackerCmd>(32);
        let tracker = Self { tx };

        tokio::spawn(async move {
            match udp::UdpClient::new().await {
                Ok(udp_client) => {
                    let actor = TrackerActor { rx };
                    actor.run(udp_client).await;
                }
                Err(e) => {
                    tracing::error!("Failed to create UDP client: {}", e);
                }
            }
        });

        tracker
    }

    pub async fn announce(
        &self,
        url: Url,
        data: AnnounceData,
    ) -> Result<AnnounceResponse, TrackerError> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(TrackerCmd::Announce {
                url,
                data,
                responder: tx,
            })
            .await;
        rx.await.map_err(|_| TrackerError::UnableToConnect)?
    }
}

struct TrackerActor {
    rx: mpsc::Receiver<TrackerCmd>,
}

impl TrackerActor {
    pub async fn run(mut self, udp_client: udp::UdpClient) {
        let http_client = http::HttpClient::new();

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                TrackerCmd::Announce {
                    url,
                    data,
                    responder,
                } => match url.scheme() {
                    "http" | "https" => {
                        let client = http_client.clone();
                        tokio::spawn(async move {
                            let result = client.announce(&url, data).await;
                            let _ = responder.send(result);
                        });
                    }
                    "udp" => {
                        let client = udp_client.clone();
                        tokio::spawn(async move {
                            let result = client.announce(&url, data).await;
                            let _ = responder.send(result);
                        });
                    }
                    _ => {
                        let _ = responder
                            .send(Err(TrackerError::InvalidScheme(url.scheme().to_string())));
                    }
                },
                TrackerCmd::Scrape { url: _, responder } => {
                    let _ = responder.send(Err(TrackerError::InvalidUrl(
                        "scrape not implemented".to_string(),
                    )));
                }
            }
        }
    }
}
