use bencode::BencodeError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DhtError {
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),

    #[error("Bencode error: {0}")]
    Bencode(#[from] BencodeError),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Invalid message: {0}")]
    InvalidMessage(String),

    #[error("Timeout waiting for response")]
    Timeout,

    #[error("Failed to bootstrap: no nodes responded")]
    BootstrapFailed,

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Channel closed")]
    ChannelClosed,

    #[error("Shutdown requested")]
    Shutdown,
}
