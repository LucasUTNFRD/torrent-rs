use async_trait::async_trait;
use std::{net::SocketAddr, time::Duration};

pub use tokio::net::{TcpListener, TcpStream};

#[async_trait]
pub trait ConnectTimeout {
    async fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> tokio::io::Result<TcpStream>;
}

#[async_trait]
impl ConnectTimeout for TcpStream {
    async fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> tokio::io::Result<Self> {
        tokio::time::timeout(timeout, async move { Self::connect(addr).await }).await?
    }
}
