use std::{net::SocketAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::Message;
use tokio::{net::TcpStream, sync::mpsc};

use crate::{bitfield::Bitfield, peer::metrics::PeerMetrics};

pub mod peer_connection;

pub mod metrics;
// Peer related

#[derive(Debug, Clone)]
pub enum PeerMessage {
    SendHave { piece_index: u32 },
    SendBitfield { bitfield: Vec<u8> },
    SendChoke,
    SendUnchoke,
    Disconnect,
    SendMessage(Message),
    HaveMetadata(Arc<Info>),
}

pub struct PeerState {
    pub addr: SocketAddr,
    pub metrics: PeerMetrics,
    pub tx: mpsc::Sender<PeerMessage>,
}

#[async_trait]
pub trait ConnectTimeout {
    async fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> tokio::io::Result<TcpStream>;
}

#[async_trait]
impl ConnectTimeout for TcpStream {
    async fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> tokio::io::Result<TcpStream> {
        tokio::time::timeout(timeout, async move { TcpStream::connect(addr).await }).await?
    }
}
