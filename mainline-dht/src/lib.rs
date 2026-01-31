//! BitTorrent Mainline DHT implementation (BEP 0005).
//!
//! This crate provides a minimal implementation of the BitTorrent DHT protocol,
//! supporting:
//! - `ping` and `find_node` queries
//! - BEP 42 secure node ID generation
//! - Iterative Kademlia-style node lookup
//!
//! # Example
//!
//! ```no_run
//! use mainline_dht::{Dht, NodeId};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create and bootstrap a DHT node
//!     let dht = Dht::new(None).await?;
//!     let node_id = dht.bootstrap().await?;
//!     println!("Our node ID: {:?}", node_id);
//!
//!     // Find nodes close to a random target
//!     let target = NodeId::generate_random();
//!     let nodes = dht.find_node(target).await?;
//!     println!("Found {} nodes", nodes.len());
//!
//!     Ok(())
//! }
//! ```

pub mod dht;
pub mod error;
pub mod message;
mod node;
pub mod node_id;
mod routing_table;

// Re-export main types for convenience
pub use dht::Dht;
pub use error::DhtError;
pub use message::CompactNodeInfo;
pub use node_id::NodeId;
