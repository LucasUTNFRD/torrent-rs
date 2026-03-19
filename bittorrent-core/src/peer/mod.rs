use crate::{bitfield::Bitfield, protocol::peer_wire::Message};
use bittorrent_common::metainfo::Info;
use std::sync::Arc;

pub mod metrics;
pub mod peer_connection;

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
}
