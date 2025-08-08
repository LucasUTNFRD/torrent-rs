use std::{
    net::{Ipv4Addr, SocketAddr, ToSocketAddrs},
    time::{Duration, Instant},
};

use bytes::{Buf, Bytes, BytesMut};
use rand::Rng;
use tokio::net::UdpSocket;
use url::Url;

use crate::types::{InfoHash, PeerID};

use super::{Actions, Events, TrackerResponse, error::TrackerError, http::AnnounceParams};

pub struct UdpTrackerClient {
    announce_url: String,
    socket: tokio::net::UdpSocket,
    server_addr: SocketAddr,
    connection: Option<UdpConnection>,
}

struct UdpConnection {
    pub connection_id: u64,
    expires_at: Instant,
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
    pub transaction_id: u32,
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
//
#[derive(Debug)]
pub struct ConnectionRequest {
    pub action: u32,
    pub transaction_id: u32,
}

impl ConnectionRequest {
    pub fn new() -> Self {
        Self {
            action: 0,
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
        buff.extend_from_slice(&self.transaction_id.to_be_bytes());
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

pub struct AnnounceResponse {}

impl UdpTrackerClient {
    pub async fn new(announce_url: &str) -> Result<Self, TrackerError> {
        let url = Url::parse(announce_url)?;

        let host = url
            .host_str()
            .ok_or_else(|| TrackerError::InvalidResponse("No host in URL".to_string()))?;
        let port = url.port().unwrap_or(80);

        let server_addr = format!("{}:{}", host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| TrackerError::InvalidResponse("Could not resolve host".to_string()))?;

        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(server_addr).await?;

        Ok(Self {
            announce_url: announce_url.to_string(),
            socket,
            server_addr,
            connection: None,
        })
    }

    async fn connect(&mut self) -> Result<(), TrackerError> {
        let request = ConnectionRequest::new();
        let request_bytes = request.to_bytes();

        self.socket.send(&request_bytes).await?;

        // Receive response
        let mut buf = BytesMut::with_capacity(16);
        let len = self.socket.recv(&mut buf).await?;

        if len < 16 {
            return Err(TrackerError::InvalidResponse(
                "Connection response too short".to_string(),
            ));
        }

        let mut buf = BytesMut::from(&buf[..len]);
        let action = buf.get_u32();
        let transaction_id = buf.get_u32();
        let connection_id = buf.get_u64();

        if action != 0 {
            return Err(TrackerError::UdpConnectionFailed);
        }

        if transaction_id != request.transaction_id {
            return Err(TrackerError::TransactionMismatch);
        }

        self.connection = Some(UdpConnection {
            connection_id,
            expires_at: Instant::now() + Duration::from_secs(120),
        });

        Ok(())
    }

    pub async fn announce(
        &mut self,
        params: &AnnounceParams,
    ) -> Result<TrackerResponse, TrackerError> {
        // Ensure we have a valid connection
        if self.connection.is_none() || self.connection.as_ref().unwrap().is_expired() {
            self.connect().await?;
        }

        let connection = self.connection.as_ref().unwrap();
        let transaction_id = rand::rng().random();

        let request = AnnounceRequest {
            connection_id: connection.connection_id,
            action: Actions::Announce,
            transaction_id,
            info_hash: params.info_hash,
            peer_id: params.peer_id,
            downloaded: params.downloaded,
            left: params.left,
            uploaded: params.uploaded,
            event: params.event.clone(),
            ip_address: Ipv4Addr::new(0, 0, 0, 0), // Default
            key: rand::rng().random(),
            num_want: -1, // Default
            port: params.port,
        };

        let request_bytes = request.to_bytes();
        self.socket.send(&request_bytes).await?;

        // Receive response
        let mut buf = BytesMut::with_capacity(1024);
        let len = self.socket.recv(&mut buf).await?;

        // self.parse_announce_response(&buf[..len], transaction_id)
        //     .await
        todo!()
    }

    async fn parse_announce_response(
        &self,
        data: &[u8],
        expected_transaction_id: u32,
    ) -> Result<TrackerResponse, TrackerError> {
        if data.len() < 20 {
            return Err(TrackerError::InvalidResponse(
                "Response too short".to_string(),
            ));
        }

        let mut buf = BytesMut::from(data);
        // std::io::Cursor::new(data);
        let action = buf.get_u32();
        let transaction_id = buf.get_u32();

        if transaction_id != expected_transaction_id {
            return Err(TrackerError::TransactionMismatch);
        }

        if action == 3 {
            // Error
            let error_msg = String::from_utf8_lossy(&data[8..]);
            return Err(TrackerError::TrackerError(error_msg.to_string()));
        }

        if action != 1 {
            // Should be announce response
            return Err(TrackerError::InvalidResponse(
                "Invalid action in response".to_string(),
            ));
        }

        let interval = buf.get_u32();
        let leechers = buf.get_u32();
        let seeders = buf.get_u32();

        // Parse peers (each peer is 6 bytes: 4 bytes IP + 2 bytes port)
        let mut peers = Vec::new();
        while buf.remaining() >= 6 {
            let ip = buf.get_u32();
            let port = buf.get_u16();
            let addr = SocketAddr::from((std::net::Ipv4Addr::from(ip), port));
            peers.push(addr);
        }

        Ok(TrackerResponse {
            peers,
            interval,
            leechers,
            seeders,
        })
    }
}

impl UdpConnection {
    fn new() -> Self {
        Self {
            connection_id: rand::rng().random(),
            expires_at: Instant::now() + Duration::from_secs(120), // 2 minutes
        }
    }

    fn is_expired(&self) -> bool {
        Instant::now() > self.expires_at
    }
}
