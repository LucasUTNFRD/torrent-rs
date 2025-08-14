use bencode::bencode::BencodeError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TrackerError {
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("Invalid tracker response: {0}")]
    InvalidResponse(String),
    #[error("Connection timeout")]
    Timeout,
    #[error("Invalid announce URL scheme: {0}")]
    InvalidScheme(String),
    #[error("Tracker returned error: {0}")]
    TrackerError(String),
    #[error("UDP connection failed")]
    UdpConnectionFailed,
    #[error("Transaction ID mismatch")]
    TransactionMismatch,
    #[error("BencodeError {0}")]
    BencodeError(BencodeError),
    #[error("Invalid string")]
    InvalidString,
    #[error("Invalid Url {0}")]
    InvalidUrl(String),
    #[error("Packet is too short")]
    TooShort,
}
