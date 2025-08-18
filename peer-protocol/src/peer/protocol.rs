use std::io::{self};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use bittorrent_core::types::{InfoHash, PeerID};

// TODO: Implement Extended Handshake Message code/decode

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub struct BlockInfo {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Block {
    pub index: u32,
    pub begin: u32,
    pub data: Bytes,
}

// #[derive(Debug)]
// pub struct BitField {
//     inner: Box<[u8]>,
//     total_pieces: usize,
// }

#[derive(Debug)]
pub enum Message {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have { piece_index: u32 },
    Bitfield(Bytes),
    Request(BlockInfo),
    Piece(Block),
    Cancel(BlockInfo),
    // Handshake(Handshake)
    // ExtendedHandshake,
}

#[derive(Debug)]
pub struct Handshake {
    pub peer_id: PeerID,
    pub info_hash: InfoHash,
    reserved: [u8; 8],
}

// handshake: <pstrlen><pstr><reserved><info_hash><peer_id>

//     pstrlen: string length of <pstr>, as a single raw byte
//     pstr: string identifier of the protocol
//     reserved: eight (8) reserved bytes. All current implementations use all zeroes. Each bit in these bytes can be used to change the behavior of the protocol. An email from Bram suggests that trailing bits should be used first, so that leading bits may be used to change the meaning of trailing bits.
//     info_hash: 20-byte SHA1 hash of the info key in the metainfo file. This is the same info_hash that is transmitted in tracker requests.
//     peer_id: 20-byte string used as a unique ID for the client. This is usually the same peer_id that is transmitted in tracker requests (but not always e.g. an anonymity option in Azureus).
impl Handshake {
    const PSTRLEN: u8 = 19;
    const PSTR: &[u8; 19] = b"BitTorrent protocol";

    pub const HANDSHAKE_LEN: usize = 68;
    pub const EXTENSION_PROTOCOL_FLAG: u8 = 0x10; // bit 43 (5th bit of 6th byte)

    pub fn new(peer_id: PeerID, info_hash: InfoHash) -> Self {
        let mut reserved = [0u8; 8];
        // Enable extension protocol support
        reserved[5] |= Self::EXTENSION_PROTOCOL_FLAG;
        Handshake {
            peer_id,
            info_hash,
            reserved,
        }
    }

    /// The bit selected for the extension protocol is bit 20 from the right (counting starts at 0). So (reserved_byte[5] & 0x10) is the expression to use for checking if the client supports extended messaging.
    pub fn support_extended_message(&self) -> bool {
        self.reserved[5] & Self::EXTENSION_PROTOCOL_FLAG != 0
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut bytes = BytesMut::with_capacity(Self::HANDSHAKE_LEN);
        bytes.put_u8(Self::PSTRLEN);
        bytes.put_slice(Self::PSTR);
        bytes.put_slice(&self.reserved);
        bytes.put_slice(self.info_hash.as_bytes());
        bytes.put_slice(self.peer_id.as_bytes());
        bytes.freeze()
    }

    //TODO: impl TryFrom<&[u8]>
    pub fn from_bytes(src: &[u8]) -> Option<Self> {
        if src.len() != Self::HANDSHAKE_LEN || src[0] != Self::PSTRLEN || &src[1..20] != Self::PSTR
        {
            return None;
        }
        let reserved: [u8; 8] = src.get(20..28)?.try_into().ok()?;
        let info_hash: [u8; 20] = src.get(28..48)?.try_into().ok()?;
        let peer_id: [u8; 20] = src.get(48..68)?.try_into().ok()?;

        Some(Handshake {
            reserved,
            peer_id: peer_id.into(),
            info_hash: info_hash.into(),
        })
    }
}

enum MessageId {
    Choke = 0,
    Unchoke = 1,
    Interested = 2,
    NotInterested = 3,
    Have = 4,
    Bitfield = 5,
    Request = 6,
    Piece = 7,
    Cancel = 8,
    ExtendedHandshake = 20,
}

impl From<u8> for MessageId {
    fn from(value: u8) -> Self {
        match value {
            k if k == MessageId::Choke as u8 => MessageId::Choke,
            k if k == MessageId::Unchoke as u8 => MessageId::Unchoke,
            k if k == MessageId::Interested as u8 => MessageId::Interested,
            k if k == MessageId::NotInterested as u8 => MessageId::NotInterested,
            k if k == MessageId::Have as u8 => MessageId::Have,
            k if k == MessageId::Bitfield as u8 => Self::Bitfield,
            k if k == MessageId::Request as u8 => MessageId::Request,
            k if k == MessageId::Piece as u8 => MessageId::Piece,
            k if k == MessageId::Cancel as u8 => Self::Cancel,
            k if k == MessageId::ExtendedHandshake as u8 => Self::ExtendedHandshake,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone)]
// Rename this PeerCodec
pub struct MessageDecoder {}

impl Decoder for MessageDecoder {
    type Item = Message;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.remaining() < 4 {
            return Ok(None);
        }

        // read without consuming
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let msg_length = u32::from_be_bytes(length_bytes);

        if src.remaining() >= 4 + msg_length as usize {
            src.advance(4);
            if msg_length == 0 {
                return Ok(Some(Message::KeepAlive));
            }
        } else {
            return Ok(None);
        }

        let msg_id = MessageId::from(src.get_u8());
        let msg = match msg_id {
            MessageId::Choke => Message::Choke,
            MessageId::Unchoke => Message::Unchoke,
            MessageId::Interested => Message::Interested,
            MessageId::NotInterested => Message::NotInterested,
            MessageId::Have => {
                let index = src.get_u32();
                Message::Have { piece_index: index }
            }
            MessageId::Bitfield => {
                let len = msg_length as usize - 1;
                let bitfield_bytes = src.split_to(len).freeze();
                Message::Bitfield(bitfield_bytes)
            }
            // <len=0013><id=6><index><begin><length>
            MessageId::Request => {
                let index = src.get_u32();
                let begin = src.get_u32();
                let length = src.get_u32();

                Message::Request(BlockInfo {
                    index,
                    begin,
                    length,
                })
            }
            // <len=0009+X><id=7><index><begin><block>
            MessageId::Piece => {
                let index = src.get_u32();
                let begin = src.get_u32();

                let data = src.split_to(msg_length as usize - 9).freeze();

                Message::Piece(Block { index, begin, data })
            }
            // <len=0013><id=8><index><begin><length>
            MessageId::Cancel => {
                let index = src.get_u32();
                let begin = src.get_u32();
                let length = src.get_u32();

                Message::Cancel(BlockInfo {
                    index,
                    begin,
                    length,
                })
            }
            MessageId::ExtendedHandshake => {
                todo!("Parse extended handshake")
            }
        };

        Ok(Some(msg))
    }
}

impl Encoder<Message> for MessageDecoder {
    type Error = std::io::Error;

    /// <length prefix><message ID><payload>. The length prefix is a four byte big-endian value. The message ID is a single decimal byte. The payload is message dependent.
    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // TODO: Avoid magic numbers use const
        match item {
            Message::KeepAlive => {
                dst.put_u32(0);
                Ok(())
            }
            Message::Choke => {
                dst.put_u32(1);
                dst.put_u8(MessageId::Choke as u8);
                Ok(())
            }
            Message::Unchoke => {
                dst.put_u32(1);
                dst.put_u8(MessageId::Unchoke as u8);
                Ok(())
            }
            Message::Interested => {
                dst.put_u32(1);
                dst.put_u8(MessageId::Interested as u8);
                Ok(())
            }
            Message::NotInterested => {
                dst.put_u32(1);
                dst.put_u8(MessageId::NotInterested as u8);
                Ok(())
            }
            Message::Have { piece_index } => {
                dst.put_u32(5);
                dst.put_u8(MessageId::Have as u8);
                dst.put_u32(piece_index);
                Ok(())
            }
            Message::Bitfield(bitfield) => {
                let length = bitfield.len() + 1;
                dst.put_u32(length as u32);
                dst.put_u8(MessageId::Bitfield as u8);
                dst.put_slice(&bitfield);
                Ok(())
            }
            Message::Request(block) => {
                dst.put_u32(13);
                dst.put_u8(MessageId::Request as u8);
                dst.put_u32(block.index);
                dst.put_u32(block.begin);
                dst.put_u32(block.length);
                Ok(())
            }
            Message::Piece(block) => {
                let length = block.data.len() + 9;
                dst.put_u32(length as u32);
                dst.put_u8(MessageId::Piece as u8);
                dst.put_u32(block.index);
                dst.put_u32(block.begin);
                dst.put_slice(&block.data);
                Ok(())
            }
            Message::Cancel(block) => {
                dst.put_u32(13);
                dst.put_u8(MessageId::Cancel as u8);
                dst.put_u32(block.index);
                dst.put_u32(block.begin);
                dst.put_u32(block.length);
                Ok(())
            }
        }
    }
}
