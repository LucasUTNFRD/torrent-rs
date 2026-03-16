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

#[derive(Debug, Clone)]
pub enum Direction {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone)]
pub enum DisconnectReason {
    Timeout,
    ProtocolError,
    Banned,
    SessionShutdown,
    Other(String),
}
