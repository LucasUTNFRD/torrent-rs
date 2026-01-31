use thiserror::Error;

#[derive(Error, Debug)]
pub enum DhtError {
    #[error("Send Error")]
    Send,
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),
}
