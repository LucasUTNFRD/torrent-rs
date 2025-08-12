use std::{
    collections::HashMap,
    io::Cursor,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
    u8,
};

use bytes::{Buf, Bytes, BytesMut};
use rand::Rng;
use tokio::{net::UdpSocket, sync::oneshot};
use url::Url;

use crate::types::{InfoHash, PeerID};

use super::{Actions, AnnounceParams, Events, TrackerClient, error::TrackerError};

#[derive(Debug)]
pub enum TrackerState {
    ConnectSent {
        txd_id: i32,
        instant: Instant,
        tx: oneshot::Sender<ConnectionResponse>,
    },
    AnnounceSent {
        txn_id: i32,
        connection_id: i64,
    },
    AnnounceReceived {
        timestamp: i64,
        num_peers: i32,
    },
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
    ) -> Result<super::TrackerResponse, TrackerError> {
        let tracker = tracker_url
            .socket_addrs(|| None)
            .map_err(|_| TrackerError::InvalidUrl(tracker_url.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| TrackerError::InvalidUrl(tracker_url.to_string()))?;
        let conn_id = self.connect(tracker).await?;
        todo!()
    }
}

/// Maximum UDP Payload (IPv4): 65,507 bytes (65,535 - 8 - 20)
const MAX_UDP_PAYLOAD_SIZE: usize = 65507;
impl UdpTrackerClient {
    pub async fn new() -> Result<Self, TrackerError> {
        let socket = UdpSocket::bind("0.0.0.0:6881").await?;
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
        let mut buf = BytesMut::with_capacity(MAX_UDP_PAYLOAD_SIZE);

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    let guard = state.write().unwrap();
                    if let Some(state) = guard.get(&addr) {
                        match state {
                            TrackerState::ConnectSent {
                                txd_id,
                                instant,
                                tx,
                            } => {
                                //Check whether the transaction ID is equal to the one you chose.
                                //Check whether the action is connect.
                                //Store the connection ID for future use.
                                if len == 16 {
                                    let packet: [u8; 16] = buf[..len].try_into().unwrap();
                                    let conn_resp = ConnectionResponse::from(packet);

                                    if conn_resp.transaction_id == txd_id
                                        && conn_resp.action == Actions::Connect as i32
                                    {
                                        let _ = tx.send(conn_resp);
                                    }

                                    buf.clear();
                                } else {
                                    tracing::error!("payload should be at least 16 bytes")
                                }
                            }
                            TrackerState::AnnounceSent {
                                txn_id,
                                connection_id,
                            } => todo!(),
                            TrackerState::AnnounceReceived {
                                timestamp,
                                num_peers,
                            } => todo!(),
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error receiving UDP packet: {}", e);
                }
            }
        }
    }

    async fn connect(&self, tracker: SocketAddr) -> Result<(), TrackerError> {
        let connect_req = ConnectionRequest::new();

        self.socket
            .send_to(&connect_req.to_bytes(), tracker)
            .await?;

        let (response_tx, response_rx) = oneshot::channel();

        {
            // Set state to connect sent
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

        // time out response_rx

        todo!()
    }
}
