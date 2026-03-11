#[cfg(feature = "sim")]
pub use turmoil::net::{TcpListener, TcpStream};

#[cfg(not(feature = "sim"))]
pub use tokio::net::{TcpListener, TcpStream};
