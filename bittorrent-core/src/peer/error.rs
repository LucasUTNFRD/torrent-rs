use thiserror::Error;

use crate::bitfield::BitfieldError;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("Network error: {0}")]
    Network(#[from] tokio::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Connection timeout")]
    #[allow(dead_code)]
    Timeout,

    #[error("Invalid handshake")]
    InvalidHandshake,

    #[error("Bitfield error")]
    BitfieldError(#[from] BitfieldError),

    #[error("Self connection detected")]
    SelfConnection,
}
