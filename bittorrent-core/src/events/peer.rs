use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub enum PeerEvent {
    Connected {
        addr: SocketAddr,
        direction: Direction,
    },
    Disconnected {
        addr: SocketAddr,
        reason: DisconnectReason,
    },
    Choked {
        addr: SocketAddr,
    },
    Unchoked {
        addr: SocketAddr,
    },
    Snubbed {
        addr: SocketAddr,
    },
    Banned {
        addr: SocketAddr,
    },
}

/// Connection direction for a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// We connected to this peer
    Outbound,
    /// This peer connected to us
    Inbound,
}

#[derive(Debug, Clone)]
pub enum DisconnectReason {
    Timeout,
    ProtocolError,
    Banned,
    SessionShutdown,
    Other(String),
}
