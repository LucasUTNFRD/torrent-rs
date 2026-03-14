use std::sync::Arc;

use bittorrent_common::metainfo::Info;
use peer_protocol::protocol::Message;

use crate::{bitfield::Bitfield, peer::metrics::PeerMetrics};

pub mod peer_connection_refactor;

pub mod metrics;

#[derive(Debug, Clone)]
pub enum PeerMessage {
    SendHave {
        piece_index: u32,
    },
    SendBitfield {
        bitfield: Bitfield,
    },
    SendChoke,
    SendUnchoke,
    #[allow(dead_code)]
    Disconnect,
    SendMessage(Message),
    HaveMetadata(Arc<Info>),
    Connected {
        metrics: Arc<PeerMetrics>,
    },
}
