use std::{net::SocketAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::{BlockInfo, Message};
use tokio::{net::TcpStream, sync::mpsc};

use crate::{bitfield::Bitfield, peer::metrics::PeerMetrics};

pub mod peer_connection;

pub mod metrics;

#[derive(Debug, Clone)]
pub enum PeerMessage {
    SendHave { piece_index: u32 },
    SendBitfield { bitfield: Bitfield },
    SendChoke,
    SendUnchoke,
    Disconnect,
    SendMessage(Message),
    HaveMetadata(Arc<Info>),
    Connected { metrics: Arc<PeerMetrics> },
}

pub struct PeerState {
    pub addr: SocketAddr,
    pub metrics: Arc<PeerMetrics>,
    pub tx: mpsc::Sender<PeerMessage>,
    pub pending_requests: Vec<BlockInfo>,
}

#[async_trait]
pub trait ConnectTimeout {
    async fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> tokio::io::Result<TcpStream>;
}

#[async_trait]
impl ConnectTimeout for TcpStream {
    async fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> tokio::io::Result<Self> {
        tokio::time::timeout(timeout, async move { Self::connect(addr).await }).await?
    }
}
