use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use bittorrent_common::types::{InfoHash, PeerID};
use bytes::{Bytes, BytesMut};
// use bytes::{Bytes, BytesMut};
use rand::Rng;
use tokio::{net::UdpSocket, sync::oneshot, time::timeout};
use url::Url;

use crate::{
    TrackerError,
    client::TrackerClient,
    types::{Actions, AnnounceParams, Events, TrackerResponse},
};

const MAX_RETRIES: usize = 8;
const CONNECTION_ID_EXPIRATION: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum TrackerState {
    ConnectSent {
        txd_id: i32,
        instant: Instant,
        tx: oneshot::Sender<ConnectionResponse>,
    },
    /// A client can use a connection ID until one minute after it has received it.
    ConnectReceived {
        connection_id: i64,
        instant: Instant,
    },
    AnnounceSent {
        txn_id: i32,
        connection_id: i64, // connection id should have a timestamp
        tx: oneshot::Sender<AnnounceResponse>,
    },
    // AnnounceReceived {
    //     re_announce_timestamp: i64,
    //     num_peers: i32,
    // },
}

// struct ConnectionId((i64, Instant));

#[derive(Debug)]
pub struct AnnounceResponse {
    action: i32, // 1 for announce
    transaction_id: i32,
    interval: i32,
    leechers: i32,
    seeders: i32,
    peers: Vec<SocketAddr>,
}

impl TryFrom<&[u8]> for AnnounceResponse {
    type Error = TrackerError;
    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        if buf.len() < 20 {
            return Err(TrackerError::TooShort);
        }

        let action = i32::from_be_bytes(buf[0..4].try_into().unwrap());
        let transaction_id = i32::from_be_bytes(buf[4..8].try_into().unwrap());
        let interval = i32::from_be_bytes(buf[8..12].try_into().unwrap());
        let leechers = i32::from_be_bytes(buf[12..16].try_into().unwrap());
        let seeders = i32::from_be_bytes(buf[16..20].try_into().unwrap());

        let mut peers = Vec::new();
        let mut offset = 20;
        while offset + 6 <= buf.len() {
            let ip = Ipv4Addr::new(
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            );
            let port = u16::from_be_bytes(buf[offset + 4..offset + 6].try_into().unwrap());
            peers.push((ip, port).into());
            offset += 6;
        }

        Ok(Self {
            action,
            transaction_id,
            interval,
            leechers,
            seeders,
            peers,
        })
    }
}

#[derive(Clone)]
pub struct UdpTrackerClient {
    socket: Arc<UdpSocket>,
    state: Arc<RwLock<HashMap<SocketAddr, TrackerState>>>,
}

const PROTOCOL_ID: u64 = 0x41727101980;

// Offset  | Size   | Name   | Value
// 0       | 64-bit |integer | connection_id
// 8       | 32-bit |integer | action          1 // announce
// 12      | 32-bit |integer | transaction_id
// 16      | 20-byte |string | info_hash
// 36      | 20-byte |string | peer_id
// 56      | 64-bit |integer | downloaded
// 64      | 64-bit |integer | left
// 72      | 64-bit |integer | uploaded
// 80      | 32-bit |integer | event           0 // 0: none; 1: completed; 2: started; 3: stopped
// 84      | 32-bit |integer | IP address      0 // default
// 88      | 32-bit |integer | key
// 92      | 32-bit |integer | num_want        -1 // default
// 96      | 16-bit |integer | port
// 98
#[derive(Debug)]
pub struct AnnounceRequest {
    pub connection_id: i64,
    pub action: Actions,
    pub tx_id: i32,
    pub info_hash: InfoHash,
    pub peer_id: PeerID,
    pub downloaded: i64,
    pub left: i64,
    pub uploaded: i64,
    pub event: Events,        // 0: none, 1: completed, 2: started, 3: stopped
    pub ip_address: Ipv4Addr, // 0 for default
    pub key: u32,
    pub num_want: i32, // -1 for default
    pub port: u16,
}

// Offset | Size           | Name           | Value
// 0      | 64-bit integer | protocol_id    | 0x41727101980 // magic constant
// 8      | 32-bit integer | action         | 0 // connect
// 12     | 32-bit integer | transaction_id
// 16
#[derive(Debug)]
pub struct ConnectionRequest {
    pub action: i32,
    pub transaction_id: i32,
}

impl ConnectionRequest {
    pub fn new() -> Self {
        Self {
            action: Actions::Connect as i32,
            transaction_id: rand::rng().random(),
        }
    }
    pub fn to_bytes(&self) -> Bytes {
        let mut buff = BytesMut::with_capacity(16);
        buff.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        buff.extend_from_slice(&self.action.to_be_bytes());
        buff.extend_from_slice(&self.transaction_id.to_be_bytes());
        buff.freeze()
    }
}

#[derive(Debug)]
pub struct ConnectionResponse {
    pub action: i32,
    pub transaction_id: i32,
    pub connection_id: i64,
}

impl From<[u8; 16]> for ConnectionResponse {
    fn from(buf: [u8; 16]) -> Self {
        let action_bytes: [u8; 4] = buf[0..4].try_into().unwrap();
        let transaction_id_bytes: [u8; 4] = buf[4..8].try_into().unwrap();
        let connection_id_bytes: [u8; 8] = buf[8..16].try_into().unwrap();

        ConnectionResponse {
            action: i32::from_be_bytes(action_bytes),
            transaction_id: i32::from_be_bytes(transaction_id_bytes),
            connection_id: i64::from_be_bytes(connection_id_bytes),
        }
    }
}

impl TryFrom<&[u8]> for ConnectionResponse {
    type Error = TrackerError;
    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        if buf.len() < 16 {
            return Err(TrackerError::TooShort);
        }
        let action_bytes: [u8; 4] = buf[0..4].try_into().unwrap();
        let transaction_id_bytes: [u8; 4] = buf[4..8].try_into().unwrap();
        let connection_id_bytes: [u8; 8] = buf[8..16].try_into().unwrap();

        Ok(ConnectionResponse {
            action: i32::from_be_bytes(action_bytes),
            transaction_id: i32::from_be_bytes(transaction_id_bytes),
            connection_id: i64::from_be_bytes(connection_id_bytes),
        })
    }
}

impl AnnounceRequest {
    pub fn to_bytes(&self) -> Bytes {
        let mut buff = BytesMut::with_capacity(98);
        buff.extend_from_slice(&self.connection_id.to_be_bytes());
        buff.extend_from_slice(&i32::from(&self.action).to_be_bytes());
        buff.extend_from_slice(&self.tx_id.to_be_bytes());
        buff.extend_from_slice(self.info_hash.as_slice());
        buff.extend_from_slice(self.peer_id.as_slice());
        buff.extend_from_slice(&self.downloaded.to_be_bytes());
        buff.extend_from_slice(&self.left.to_be_bytes());
        buff.extend_from_slice(&self.uploaded.to_be_bytes());
        buff.extend_from_slice(&i32::from(&self.event).to_be_bytes());
        buff.extend_from_slice(&self.ip_address.to_bits().to_be_bytes());
        buff.extend_from_slice(&self.key.to_be_bytes());
        buff.extend_from_slice(&self.num_want.to_be_bytes());
        buff.extend_from_slice(&self.port.to_be_bytes());
        buff.freeze()
    }
}

#[async_trait::async_trait]
impl TrackerClient for UdpTrackerClient {
    async fn announce(
        &self,
        params: &AnnounceParams,
        tracker_url: Url,
    ) -> Result<TrackerResponse, TrackerError> {
        // this should be a method that resolves a url into a socket addrs
        let tracker = tracker_url
            .socket_addrs(|| None)
            .map_err(|_| TrackerError::InvalidUrl(tracker_url.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| TrackerError::InvalidUrl(tracker_url.to_string()))?;

        println!("tracker socket address {tracker:?}");

        println!("tryng to establish connection to {tracker:?}");
        let connection_id = self.connect(tracker).await?;
        tracing::debug!("established connection to {tracker:?}");
        for n in 0..=MAX_RETRIES {
            println!("Trying for n = {n}");
            let tx_id = rand::rng().random();
            let announce_req = AnnounceRequest {
                connection_id,
                action: Actions::Announce,
                tx_id,
                info_hash: params.info_hash,
                peer_id: params.peer_id,
                downloaded: params.downloaded,
                left: params.left,
                uploaded: params.uploaded,
                event: params.event,
                ip_address: Ipv4Addr::from(0), // default
                key: 0,                        // default
                num_want: -1,                  // default
                port: params.port,
            };

            let (tx, rx) = oneshot::channel();

            {
                self.socket
                    .send_to(&announce_req.to_bytes(), tracker)
                    .await?;
                tracing::debug!("sent request to tracker");
                let mut guard = self.state.write().unwrap();
                guard.insert(
                    tracker,
                    TrackerState::AnnounceSent {
                        txn_id: tx_id,
                        tx,
                        connection_id: announce_req.connection_id,
                    },
                );
            }

            let wait_secs = 15 * (1 << n);
            match timeout(Duration::from_secs(wait_secs), rx).await {
                Ok(Ok(resp)) => {
                    return Ok(TrackerResponse {
                        peers: resp.peers,
                        interval: resp.interval,
                        leechers: resp.leechers,
                        seeders: resp.seeders,
                    });
                }
                Ok(Err(_)) => tracing::error!("Oneshot channel closed unexpectedly"),
                Err(_) => tracing::warn!("Connect request timed out (attempt {})", n + 1),
            }
        }
        Err(TrackerError::Timeout)
    }
}

/// Maximum UDP Payload (IPv4): 65,507 bytes (65,535 - 8 - 20)
const MAX_UDP_PAYLOAD_SIZE: usize = 65507;
impl UdpTrackerClient {
    pub async fn new() -> Result<Self, TrackerError> {
        let socket = UdpSocket::bind("0:0").await?; // Use random available port
        // let socket = UdpSocket::bind("0.0.0.0:6881").await?;
        let socket = Arc::new(socket);

        let state = Arc::new(RwLock::new(HashMap::new()));

        let client = Self {
            socket: socket.clone(),
            state: state.clone(),
        };

        tokio::spawn(Self::reader_task(socket.clone(), state.clone()));

        Ok(client)
    }

    async fn reader_task(
        socket: Arc<UdpSocket>,
        state: Arc<RwLock<HashMap<SocketAddr, TrackerState>>>,
    ) {
        // let mut buf = BytesMut::with_capacity(MAX_UDP_PAYLOAD_SIZE);
        let mut buf = vec![0u8; MAX_UDP_PAYLOAD_SIZE];

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    println!("received {:?}", &buf[..len]);
                    let mut guard = state.write().unwrap();
                    println!("adcquired lock");
                    if let Some(state) = guard.remove(&addr) {
                        println!("Current state{state:?}");
                        // we need ownership
                        // of state
                        match state {
                            TrackerState::ConnectSent {
                                txd_id,
                                instant,
                                tx,
                            } => {
                                if let Ok(conn_resp) = ConnectionResponse::try_from(&buf[..len]) {
                                    //Check whether the transaction ID is equal to the one you chose.
                                    //     //Check whether the action is connect.
                                    if conn_resp.transaction_id == txd_id
                                        && conn_resp.action == Actions::Connect as i32
                                    {
                                        // Switch state to ConnectionReceived
                                        guard.insert(
                                            addr,
                                            TrackerState::ConnectReceived {
                                                connection_id: conn_resp.connection_id,
                                                instant: Instant::now(),
                                            },
                                        );
                                        let _ = tx.send(conn_resp);
                                    }
                                } else {
                                    guard.insert(
                                        addr,
                                        TrackerState::ConnectSent {
                                            txd_id,
                                            instant,
                                            tx,
                                        },
                                    );
                                    tracing::error!("payload should be at least 16 bytes")
                                }
                            }
                            TrackerState::AnnounceSent {
                                txn_id,
                                connection_id,
                                tx,
                            } => {
                                if let Ok(announce_resp) = AnnounceResponse::try_from(&buf[..len]) {
                                    if announce_resp.transaction_id == txn_id
                                        && announce_resp.action == Actions::Announce as i32
                                    {
                                        let _ = tx.send(announce_resp);
                                    }
                                } else {
                                    // Mantain state
                                    guard.insert(
                                        addr,
                                        TrackerState::AnnounceSent {
                                            txn_id,
                                            connection_id,
                                            tx,
                                        },
                                    );
                                }
                            }
                            _ => panic!("invalid state"),
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error receiving UDP packet: {}", e);
                }
            }
        }
    }

    fn is_expired(instant: Instant) -> bool {
        Instant::now() - instant >= CONNECTION_ID_EXPIRATION
    }

    async fn connect(&self, tracker: SocketAddr) -> Result<i64, TrackerError> {
        // Check if we are already connected to this peer
        println!(" DEBUG: Starting connection process to {}", tracker);
        {
            let guard = self.state.read().unwrap();

            #[allow(clippy::collapsible_if)]
            if let Some(TrackerState::ConnectReceived {
                connection_id,
                instant,
            }) = guard.get(&tracker)
            {
                if !Self::is_expired(*instant) {
                    println!(" DEBUG: Reusing existing connection_id: {}", connection_id);
                    return Ok(*connection_id);
                }
            }
        }
        for n in 0..=MAX_RETRIES {
            println!(
                " DEBUG: Connection attempt {} of {}",
                n + 1,
                MAX_RETRIES + 1
            );
            let connect_req = ConnectionRequest::new();
            println!("📤 DEBUG: Sending connection request {connect_req:?}");
            let sent = self
                .socket
                .send_to(&connect_req.to_bytes(), tracker)
                .await?;
            println!("📤 DEBUG: Sent {} bytes to tracker", sent);

            let (response_tx, response_rx) = oneshot::channel();

            {
                // Set state to ConnectSent
                let mut guard = self.state.write().unwrap();
                guard.insert(
                    tracker,
                    TrackerState::ConnectSent {
                        txd_id: connect_req.transaction_id,
                        instant: Instant::now(),
                        tx: response_tx,
                    },
                );
            }

            let wait_secs = 15 * (1 << n);
            match timeout(Duration::from_secs(wait_secs), response_rx).await {
                Ok(Ok(conn_resp)) => return Ok(conn_resp.connection_id),
                Ok(Err(_)) => tracing::error!("Oneshot channel closed unexpectedly"),
                Err(_) => tracing::warn!("Connect request timed out (attempt {})", n + 1),
            }
        }

        Err(TrackerError::Timeout)
    }
}
