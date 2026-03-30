use thiserror::Error;

use crate::bitfield::BitfieldError;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("I/O error: {0}")]
    Io(#[from] tokio::io::Error),

    #[error("Handshake timed out")]
    HandshakeTimeout,

    #[error("Write stalled")]
    WriteStalled,

    #[error("Handshake failed: {0}")]
    Handshake(#[from] HandshakeError),

    #[error("Protocol violation: {0}")]
    Protocol(#[from] ProtocolViolation),

    #[error("Bitfield error: {0}")]
    Bitfield(#[from] BitfieldError),

    #[error("Self-connection detected")]
    SelfConnection,

    #[error("Idle timeout")]
    Idle,
}

impl ConnectionError {
    /// Whether this error is transient (worth retrying) or permanent.
    ///
    /// Transient errors are typically network-level issues that may resolve
    /// on a subsequent attempt. Permanent errors indicate protocol violations,
    /// identity mismatches, or logical errors that won't change on retry.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Io(io_err) => matches!(
                io_err.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::WouldBlock
            ),
            Self::HandshakeTimeout | Self::WriteStalled | Self::Idle => true,
            Self::Handshake(_) | Self::Protocol(_) | Self::Bitfield(_) | Self::SelfConnection => {
                false
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error("Parse failure: incomplete or malformed bytes")]
    ParseFailure,
    #[error("Info hash mismatch")]
    InfoHashMismatch,
}

#[derive(Debug, Error)]
pub enum ProtocolViolation {
    #[error("Duplicate bitfield")]
    DuplicateBitfield,
    #[error("HAVE piece index {index} out of range (have {total} pieces)")]
    InvalidPieceIndex { index: u32, total: u32 },
    #[error("Unknown metadata msg_type: {0}")]
    UnknownMetadataMsgType(i64),
    #[error("Bad metadata bencode: {0}")]
    BadMetadataBencode(String),
    #[error("Missing field in metadata message: {0}")]
    MissingMetadataField(&'static str),
}
