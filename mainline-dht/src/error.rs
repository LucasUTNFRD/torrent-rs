use thiserror::Error;

#[derive(Error, Debug)]
pub enum DhtError {
    #[error("Send Error")]
    Send,
}
