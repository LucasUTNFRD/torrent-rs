use std::{net::SocketAddr, sync::Arc};

use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::Message;
use tokio::sync::mpsc;

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
