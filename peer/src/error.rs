use thiserror::Error;

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("Peer disconnected")]
    Disconnected,
    #[error("Connection timeout")]
    Timeout,
    #[error("I/O error: {0}")]
    IoError(std::io::Error),
    #[error("Invalid handshake")]
    InvalidHandshake,
}
