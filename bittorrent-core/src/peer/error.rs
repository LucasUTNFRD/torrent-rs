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
