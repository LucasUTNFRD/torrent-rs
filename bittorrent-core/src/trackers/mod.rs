use std::{net::SocketAddr, sync::Arc, time::Duration};

use bencode::BencodeError;
use thiserror::Error;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
};
use url::Url;

mod udp;

pub enum Events {
    None,
    Completed,
    Started,
    Stopped,
}

struct ClientCtx {}

enum TrackerCmd {
    Announce(Request<AnnounceRequest, AnnounceResponse>),
    Scrape(Request<ScrapeRequest, ScrapeResponse>),
}
struct AnnounceRequest {}
struct AnnounceResponse {
    /// A list of peer addresses (IP and port) obtained from the tracker.
    /// These are potential peers for the client to connect to for downloading/uploading data.
    pub peers: Vec<SocketAddr>,
    /// The recommended interval (in seconds) that the client should wait
    /// before sending its next regular announce request to the tracker.
    pub interval: i32,
    /// The number of "leechers" (incomplete peers) currently in the swarm for this torrent.
    pub leechers: i32,
    /// The number of "seeders" (complete peers) currently in the swarm for this torrent.
    pub seeders: i32,
}

struct ScrapeRequest {}
struct ScrapeResponse {}

struct Request<TrackerRequest, TrackerResponse> {
    url: TrackerUrl,
    request: TrackerRequest,
    responder: oneshot::Sender<Result<TrackerResponse, TrackerError>>,
}

struct Tracker {
    tx: mpsc::Sender<TrackerCmd>,
}

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

enum TrackerUrl {
    Udp(String),
    Http(String),
}

impl Tracker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<TrackerCmd>(32);
        Self { tx }
    }

    pub async fn announce(
        &self,
        url: TrackerUrl,
        request: AnnounceRequest,
    ) -> Result<AnnounceResponse, TrackerError> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(TrackerCmd::Announce(Request {
                url,
                request,
                responder: tx,
            }))
            .await;
        rx.await.unwrap()
    }

    /// Send scrape request to a tracker and wait for response.
    pub async fn scrape(
        &self,
        url: TrackerUrl,
        request: ScrapeRequest,
    ) -> Result<ScrapeResponse, TrackerError> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(TrackerCmd::Scrape(Request {
                url,
                request,
                responder: tx,
            }))
            .await;
        rx.await.unwrap()
    }
}

struct TrackerActor {
    rx: mpsc::Receiver<TrackerCmd>,
}

impl TrackerActor {
    pub async fn run(mut self) {
        let http_client = HttpClient(
            reqwest::Client::builder()
                .gzip(true)
                .timeout(Duration::from_secs(25))
                .build()
                .inspect_err(|e| tracing::warn!(?e, "proceeding without http client"))
                .unwrap(),
        );

        let udp_socket = {
            let socket = UdpSocket::bind("0:0").await.unwrap();
            Arc::new(socket)
        };

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                TrackerCmd::Announce(req) => match req.url {
                    TrackerUrl::Udp(url) => {}
                    TrackerUrl::Http(url) => {
                        let client = http_client.clone();
                        tokio::spawn(async move {
                            let result = client.announce(url, req.request).await;
                            req.responder.send(result)
                        });
                    }
                },
                TrackerCmd::Scrape(req) => {}
            }
        }
    }
}

#[derive(Clone)]
struct HttpClient(reqwest::Client);

impl HttpClient {
    async fn announce(
        &self,
        url: String,
        req: AnnounceRequest,
    ) -> Result<AnnounceResponse, TrackerError> {
        // check url
        let url = Url::parse(&url).expect("Url parse failure"); // TODO: Create
        let announce_url = todo!();
    }
}
