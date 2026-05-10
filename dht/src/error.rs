use bencode::BencodeError;

#[derive(Debug, thiserror::Error)]
pub enum DhtError {
    #[error("socket error: {0}")]
    Socket(#[from] std::io::Error),

    #[error("invalid address family on {0} socket")]
    WrongFamily(&'static str),

    #[error("DHT service unavailable")]
    ServiceUnavailable,

    #[error("IO error: {0}")]
    IO(String),

    #[error("operation cancelled")]
    Cancelled,

    #[error("Bencode error: {0}")]
    Bencode(#[from] BencodeError),

    #[error("Parse error: {0}")]
    Parse(String),
}
