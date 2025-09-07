use std::collections::HashMap;

use bytes::Bytes;

// TODO: Implement ExtendedHandshake

/// [docs](https://www.libtorrent.org/extension_protocol.html)
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedHandshake {
    /// Dictionary of supported extension messages which maps names of
    /// extensions to an extended message ID
    pub m: Option<HashMap<String, i64>>,
    /// Client name and version (as a utf-8 string). This is a much
    /// more reliable way of identifying the client than relying on
    /// the peer id encoding.
    pub v: Option<String>,
    /// The number of outstanding request messages this client supports
    /// without dropping any. The default in in libtorrent is 250.
    pub reqq: Option<i64>,
    /// Local TCP listen port. Allows each side to learn about the TCP
    /// port number of the other side. Note that there is no need for
    /// the receiving side of the connection to send this extension
    /// message, since its port number is already known.
    pub p: Option<i64>,
    /// A string containing the compact representation of the ip address
    /// this peer sees you as
    pub yourip: Option<Bytes>,
    /// If this peer has an IPv4 interface, this is the compact
    /// representation of that address (4 bytes). The client may prefer
    /// to connect back via the IPv6 address.
    pub ipv4: Option<Bytes>,
    /// If this peer has an IPv6 interface, this is the compact
    /// representation of that address (16 bytes). The client may prefer
    /// to connect back via the IPv6 address.
    pub ipv6: Option<Bytes>,
    pub metadata_size: Option<i64>,
    pub upload_only: Option<i64>,
    pub ut_holepunch: Option<i64>,
    pub lt_donthave: Option<i64>,
    /// the time when this peer last saw a complete copy
    /// of this torrent
    pub complete_ago: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExtendedMessage {
    /// Extended handshake (ID 0)
    Handshake(ExtendedHandshake),
    /// Generic extension message with custom ID and payload
    Extension { id: u8, payload: Bytes },
    /// ut_metadata extension messages
    Metadata(MetadataMessage),
    /// lt_donthave extension message
    DontHave { piece_index: u32 },
}

/// ut_metadata extension message types
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataMessage {
    /// Request metadata piece
    Request { piece: u32 },
    /// Metadata piece data
    Data {
        piece: u32,
        total_size: Option<u32>,
        data: Bytes,
    },
    /// Reject metadata request
    Reject { piece: u32 },
}
