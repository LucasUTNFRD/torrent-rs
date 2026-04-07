use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use rand::RngExt;
use tokio::{
    net::UdpSocket,
    sync::{RwLock, oneshot},
    time::timeout,
};

use super::{AnnounceData, AnnounceResponse, TrackerError};

const PROTOCOL_ID: u64 = 0x41727101980;
const CONNECTION_EXPIRATION: Duration = Duration::from_secs(60);
const MAX_RETRIES: usize = 8;
const MAX_UDP_PAYLOAD: usize = 65507;

#[derive(Clone)]
pub struct UdpClient {
    socket: Arc<UdpSocket>,
    pending: Arc<RwLock<HashMap<i32, oneshot::Sender<ResponseKind>>>>,
    connections: Arc<RwLock<HashMap<SocketAddr, CachedConnection>>>,
}

struct CachedConnection {
    id: i64,
    expires_at: Instant,
}

enum ResponseKind {
    Connect(ConnectionResponse),
    Announce(AnnounceResponse),
}

struct ConnectionResponse {
    connection_id: i64,
}

struct AnnounceResponseInternal {
    interval: i32,
    leechers: i32,
    seeders: i32,
    peers: Vec<SocketAddr>,
}

impl TryFrom<&[u8]> for AnnounceResponseInternal {
    type Error = TrackerError;

    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        if buf.len() < 20 {
            return Err(TrackerError::TooShort);
        }

        let action = i32::from_be_bytes(buf[0..4].try_into().unwrap());
        let _transaction_id = i32::from_be_bytes(buf[4..8].try_into().unwrap());
        let interval = i32::from_be_bytes(buf[8..12].try_into().unwrap());
        let leechers = i32::from_be_bytes(buf[12..16].try_into().unwrap());
        let seeders = i32::from_be_bytes(buf[16..20].try_into().unwrap());

        if action != 1 {
            return Err(TrackerError::InvalidResponse("not announce action".into()));
        }

        let mut peers = Vec::new();
        let mut offset = 20;
        while offset + 6 <= buf.len() {
            let ip = Ipv4Addr::new(
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            );
            let port = u16::from_be_bytes([buf[offset + 4], buf[offset + 5]]);
            peers.push(SocketAddr::new(ip.into(), port));
            offset += 6;
        }

        Ok(Self {
            interval,
            leechers,
            seeders,
            peers,
        })
    }
}

impl TryFrom<&[u8]> for ConnectionResponse {
    type Error = TrackerError;

    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        if buf.len() < 16 {
            return Err(TrackerError::TooShort);
        }

        let action = i32::from_be_bytes(buf[0..4].try_into().unwrap());
        if action != 0 {
            return Err(TrackerError::InvalidResponse("not connect action".into()));
        }

        Ok(Self {
            connection_id: i64::from_be_bytes(buf[8..16].try_into().unwrap()),
        })
    }
}

impl UdpClient {
    pub async fn new() -> Result<Self, TrackerError> {
        let socket = Arc::new(UdpSocket::bind("0:0").await?);
        let pending = Arc::new(RwLock::new(HashMap::new()));
        let connections = Arc::new(RwLock::new(HashMap::new()));

        tokio::spawn(Self::recv_loop(socket.clone(), pending.clone()));

        Ok(Self {
            socket,
            pending,
            connections,
        })
    }

    async fn recv_loop(
        socket: Arc<UdpSocket>,
        pending: Arc<RwLock<HashMap<i32, oneshot::Sender<ResponseKind>>>>,
    ) {
        let mut buf = BytesMut::zeroed(MAX_UDP_PAYLOAD);

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, _addr)) => {
                    if len < 4 {
                        continue;
                    }

                    let transaction_id = i32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                    let action = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);

                    let response = if action == 0 {
                        ConnectionResponse::try_from(&buf[..len])
                            .ok()
                            .map(ResponseKind::Connect)
                    } else if action == 1 {
                        AnnounceResponseInternal::try_from(&buf[..len])
                            .ok()
                            .map(|r| {
                                ResponseKind::Announce(AnnounceResponse {
                                    peers: r.peers,
                                    interval: r.interval,
                                    leechers: r.leechers,
                                    seeders: r.seeders,
                                })
                            })
                    } else {
                        None
                    };

                    if let Some(resp) = response {
                        let mut guard = pending.write().await;
                        if let Some(tx) = guard.remove(&transaction_id) {
                            let _ = tx.send(resp);
                        }
                    }
                }
                Err(e) => tracing::error!("UDP recv error: {}", e),
            }
        }
    }

    pub async fn announce(
        &self,
        url: &str,
        data: AnnounceData,
    ) -> Result<AnnounceResponse, TrackerError> {
        let addr = parse_udp_url(url)?;

        let connection_id = self.get_connection(addr).await?;

        for attempt in 0..=MAX_RETRIES {
            let tx_id: i32 = rand::rng().random();
            let request = build_announce_request(connection_id, tx_id, &data);

            let (tx, rx) = oneshot::channel();
            {
                let mut guard = self.pending.write().await;
                guard.insert(tx_id, tx);
            }

            self.socket.send_to(&request, addr).await?;

            let wait_secs = 15 * (1 << attempt);
            match timeout(Duration::from_secs(wait_secs), rx).await {
                Ok(Ok(ResponseKind::Announce(resp))) => {
                    return Ok(resp);
                }
                Ok(Ok(ResponseKind::Connect(_))) => {
                    tracing::warn!("Unexpected connect response for announce request");
                }
                Ok(Err(_)) => {
                    tracing::warn!("Announce response channel closed");
                }
                Err(_) => {
                    tracing::warn!(attempt = attempt + 1, "Announce request timed out");
                }
            }

            self.pending.write().await.remove(&tx_id);
        }

        Err(TrackerError::Timeout)
    }

    async fn get_connection(&self, addr: SocketAddr) -> Result<i64, TrackerError> {
        if let Some(cached) = self.connections.read().await.get(&addr)
            && cached.expires_at > Instant::now()
        {
            return Ok(cached.id);
        }

        for attempt in 0..=MAX_RETRIES {
            let tx_id: i32 = rand::rng().random();
            let request = build_connect_request(tx_id);

            let (tx, rx) = oneshot::channel();
            {
                let mut guard = self.pending.write().await;
                guard.insert(tx_id, tx);
            }

            self.socket.send_to(&request, addr).await?;

            let wait_secs = 15 * (1 << attempt);
            match timeout(Duration::from_secs(wait_secs), rx).await {
                Ok(Ok(ResponseKind::Connect(resp))) => {
                    self.connections.write().await.insert(
                        addr,
                        CachedConnection {
                            id: resp.connection_id,
                            expires_at: Instant::now() + CONNECTION_EXPIRATION,
                        },
                    );
                    self.pending.write().await.remove(&tx_id);
                    return Ok(resp.connection_id);
                }
                Ok(Ok(ResponseKind::Announce(_))) => {
                    tracing::warn!("Unexpected announce response for connect request");
                }
                Ok(Err(_)) => {
                    tracing::warn!("Connect response channel closed");
                }
                Err(_) => {
                    tracing::warn!(attempt = attempt + 1, "Connect request timed out");
                }
            }

            self.pending.write().await.remove(&tx_id);
        }

        Err(TrackerError::Timeout)
    }
}

fn parse_udp_url(url: &str) -> Result<SocketAddr, TrackerError> {
    let url = url::Url::parse(url).map_err(|_| TrackerError::InvalidUrl(url.to_string()))?;

    let host = url
        .host_str()
        .ok_or_else(|| TrackerError::InvalidUrl(url.to_string()))?;
    let port = url
        .port()
        .ok_or_else(|| TrackerError::InvalidUrl(url.to_string()))?;

    use std::net::ToSocketAddrs;
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|_| TrackerError::InvalidUrl(url.to_string()))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| TrackerError::InvalidUrl(url.to_string()))?;

    Ok(addr)
}

fn build_connect_request(tx_id: i32) -> Bytes {
    let mut buf = BytesMut::with_capacity(16);
    buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
    buf.extend_from_slice(&0i32.to_be_bytes());
    buf.extend_from_slice(&tx_id.to_be_bytes());
    buf.freeze()
}

fn build_announce_request(connection_id: i64, tx_id: i32, data: &AnnounceData) -> Bytes {
    let mut buf = BytesMut::with_capacity(98);
    buf.extend_from_slice(&connection_id.to_be_bytes());
    buf.extend_from_slice(&1i32.to_be_bytes());
    buf.extend_from_slice(&tx_id.to_be_bytes());
    buf.extend_from_slice(data.info_hash.as_slice());
    buf.extend_from_slice(data.peer_id.as_slice());
    buf.extend_from_slice(&data.downloaded.to_be_bytes());
    buf.extend_from_slice(&data.left.to_be_bytes());
    buf.extend_from_slice(&data.uploaded.to_be_bytes());
    buf.extend_from_slice(&data.event.as_int().to_be_bytes());
    buf.extend_from_slice(&0u32.to_be_bytes());
    buf.extend_from_slice(&0u32.to_be_bytes());
    buf.extend_from_slice(&(-1i32).to_be_bytes());
    buf.extend_from_slice(&data.port.to_be_bytes());
    buf.freeze()
}
