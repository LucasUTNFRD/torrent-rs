use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
    u8,
};

use bytes::{Buf, Bytes, BytesMut};
use rand::Rng;
use tokio::net::UdpSocket;
use url::Url;

use crate::types::{InfoHash, PeerID};

use super::{Actions, AnnounceParams, Events, TrackerClient, error::TrackerError};

#[derive(Debug, Clone)]
pub enum TrackerState {
    ConnectSent { txd_id: i32 },
    AnnounceSent { txn_id: i32, connection_id: i64 },
    AnnounceReceived { timestamp: i64, num_peers: i32 },
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
    pub connection_id: u64,
    pub action: Actions,
    pub tx_id: u32,
    pub info_hash: InfoHash,
    pub peer_id: PeerID,
    pub downloaded: u64,
    pub left: u64,
    pub uploaded: u64,
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
    pub action: u32,
    pub transaction_id: i32,
}

impl ConnectionRequest {
    pub fn new() -> Self {
        Self {
            action: Actions::Connect as u32,
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

impl AnnounceRequest {
    pub fn to_bytes(&self) -> Bytes {
        let mut buff = BytesMut::with_capacity(98);
        buff.extend_from_slice(&self.connection_id.to_be_bytes());
        buff.extend_from_slice(&u32::from(&self.action).to_be_bytes());
        buff.extend_from_slice(&self.tx_id.to_be_bytes());
        buff.extend_from_slice(self.info_hash.as_slice());
        buff.extend_from_slice(self.peer_id.as_slice());
        buff.extend_from_slice(&self.downloaded.to_be_bytes());
        buff.extend_from_slice(&self.left.to_be_bytes());
        buff.extend_from_slice(&self.uploaded.to_be_bytes());
        buff.extend_from_slice(&u32::from(&self.event).to_be_bytes());
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
                    Self::process_msg(&buf[..len], addr, state.clone()).await;
                }
                Err(e) => {
                    eprintln!("Error receiving UDP packet: {}", e);
                }
            }
        }
    }

    async fn process_msg(
        buf: &[u8],
        addr: SocketAddr,
        state: Arc<RwLock<HashMap<SocketAddr, TrackerState>>>,
    ) {
        todo!()
    }

    async fn connect(&self, tracker: SocketAddr) -> Result<(), TrackerError> {
        let connect_req = ConnectionRequest::new();
        {
            // Set state to connect sent
            let mut guard = self.state.write().unwrap();
            guard.entry(tracker).or_insert(TrackerState::ConnectSent {
                txd_id: connect_req.transaction_id,
            });
        }
        self.socket
            .send_to(&connect_req.to_bytes(), tracker)
            .await?;
        todo!()
    }
}
