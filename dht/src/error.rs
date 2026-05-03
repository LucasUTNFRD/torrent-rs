#[derive(Debug, thiserror::Error)]
pub enum DhtError {
    #[error("socket error: {0}")]
    Socket(#[from] std::io::Error),

    #[error("invalid address family on {0} socket")]
    WrongFamily(&'static str),
}
