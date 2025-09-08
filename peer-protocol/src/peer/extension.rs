use std::collections::BTreeMap;

use bencode::{Bencode, BencodeBuilder, Encode};
use bytes::Bytes;

// TODO: Implement ExtendedHandshake

/// [docs](https://www.libtorrent.org/extension_protocol.html)
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedHandshake {
    /// Dictionary of supported extension messages which maps names of
    /// extensions to an extended message ID
    pub m: Option<BTreeMap<String, i64>>,
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
}

impl Encode for ExtendedHandshake {
    fn to_bencode(&self) -> Bencode {
        let mut dict = BTreeMap::new();

        // Handle the extension dictionary (m field)
        dict.insert_optional("m", &self.m);

        // Handle other optional fields
        dict.insert_optional("v", &self.v);
        dict.insert_optional("reqq", &self.reqq);
        dict.insert_optional("p", &self.p);
        dict.insert_optional("yourip", &self.yourip);
        dict.insert_optional("ipv4", &self.ipv4);
        dict.insert_optional("ipv6", &self.ipv6);
        dict.insert_optional("metadata_size", &self.metadata_size);

        dict.build()
    }
}

impl ExtendedHandshake {
    /// Create a new ExtendedHandshake with default values
    pub fn new() -> Self {
        Self {
            m: None,
            v: None,
            reqq: None,
            p: None,
            yourip: None,
            ipv4: None,
            ipv6: None,
            metadata_size: None,
        }
    }

    /// Builder pattern methods for easy construction
    pub fn with_extensions(mut self, extensions: BTreeMap<String, i64>) -> Self {
        self.m = Some(extensions);
        self
    }

    pub fn with_client_version<S: Into<String>>(mut self, version: S) -> Self {
        self.v = Some(version.into());
        self
    }

    pub fn with_request_queue_size(mut self, size: i64) -> Self {
        self.reqq = Some(size);
        self
    }

    /// Set the IP address this peer sees you as (compact representation)
    pub fn with_yourip(mut self, ip: Bytes) -> Self {
        self.yourip = Some(ip);
        self
    }

    /// Set IPv4 address (4 bytes compact representation)
    pub fn with_ipv4(mut self, ip: Bytes) -> Self {
        self.ipv4 = Some(ip);
        self
    }

    /// Set IPv6 address (16 bytes compact representation)  
    pub fn with_ipv6(mut self, ip: Bytes) -> Self {
        self.ipv6 = Some(ip);
        self
    }

    pub fn with_port(mut self, port: i64) -> Self {
        self.p = Some(port);
        self
    }

    pub fn with_metadata_size(mut self, size: i64) -> Self {
        self.metadata_size = Some(size);
        self
    }
}

impl Default for ExtendedHandshake {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExtendedMessage {
    /// Extended handshake (ID 0)
    Handshake(ExtendedHandshake),
    // /// Generic extension message with custom ID and payload
    // Extension { id: u8, payload: Bytes },
    // /// ut_metadata extension messages
    // Metadata(MetadataMessage),
    // /// lt_donthave extension message
    // DontHave { piece_index: u32 },
}

// /// ut_metadata extension message types
// #[derive(Debug, Clone, PartialEq)]
// pub enum MetadataMessage {
//     /// Request metadata piece
//     Request { piece: u32 },
//     /// Metadata piece data
//     Data {
//         piece: u32,
//         total_size: Option<u32>,
//         data: Bytes,
//     },
//     /// Reject metadata request
//     Reject { piece: u32 },
// }

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;

    use bencode::Bencode;

    use crate::peer::extension::ExtendedHandshake;

    ///     An example of what the payload of a handshake message could look like:
    ///
    /// +------------------------------------------------------+
    /// | Dictionary                                           |
    /// +===================+==================================+
    /// | ``m``             |  +--------------------------+    |
    /// |                   |  | Dictionary               |    |
    /// |                   |  +======================+===+    |
    /// |                   |  | ``LT_metadata``      | 1 |    |
    /// |                   |  +----------------------+---+    |
    /// |                   |  | ``ut_pex``           | 2 |    |
    /// |                   |  +----------------------+---+    |
    /// |                   |                                  |
    /// +-------------------+----------------------------------+
    /// | ``p``             | 6881                             |
    /// +-------------------+----------------------------------+
    /// | ``v``             | "uTorrent 1.2"                   |
    /// +-------------------+----------------------------------+
    ///
    /// and in the encoded form:
    ///
    /// ``d1:md11:LT_metadatai1e6:ut_pexi2ee1:pi6881e1:v12:uTorrent 1.2e``

    #[test]
    fn test_extended_message_payload_encoding() {
        let mut extensions = BTreeMap::new();
        extensions.insert("LT_metadata".to_string(), 1);
        extensions.insert("ut_pex".to_string(), 2);

        let handshake = ExtendedHandshake::new()
            .with_extensions(extensions)
            .with_port(6881)
            .with_client_version("uTorrent 1.2");

        let encoded = Bencode::encode(&handshake);
        let encoded_str = String::from_utf8(encoded).expect("Should be valid UTF-8");

        let expected_handshake = "d1:md11:LT_metadatai1e6:ut_pexi2ee1:pi6881e1:v12:uTorrent 1.2e";

        assert_eq!(expected_handshake, encoded_str);
    }
}
