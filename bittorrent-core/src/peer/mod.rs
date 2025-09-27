use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64},
    },
};

use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::Message;
use tokio::sync::mpsc;

pub mod peer_connection;

mod metrics;
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
    // pub(crate) bitfield: Bitfield,
    pub tx: mpsc::Sender<PeerMessage>,
}
