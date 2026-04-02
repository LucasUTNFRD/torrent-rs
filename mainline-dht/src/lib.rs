//! BitTorrent Mainline DHT implementation (BEP 0005).
//!
//! This crate provides a minimal implementation of the BitTorrent DHT protocol,
//! supporting:
//! - `ping` and `find_node` queries
//! - `get_peers` and `announce_peer` queries
//! - BEP 42 secure node ID generation
//! - Iterative Kademlia-style node lookup
//! - Automatic bootstrap on startup
//!
//! # Example
//!
//! ```no_run
//! use mainline_dht::{Dht, DhtConfig, NodeId};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a DHT node (bootstrap happens automatically)
//!     let config = DhtConfig::builder().port(6881).build();
//!     let dht = Dht::with_config(config).await?;
//!     println!("Our node ID: {:?}", dht.node_id());
//!
//!     // Find nodes close to a random target
//!     let target = NodeId::generate_random();
//!
//!     Ok(())
//! }
//! ```

pub mod dht;
mod dht_config;
pub mod error;
pub mod message;
mod metrics;
mod node;
pub mod node_id;
mod peer_store;
mod routing_table;
mod token;
pub mod transaction;

// Re-export main types for convenience
pub use dht::{DhtHandler, DhtResponse, GetPeersResult};
pub use dht_config::{BootstrapNode, DhtConfig, DhtConfigBuilder, default_dht_bootstrap_nodes};

pub type Dht = DhtHandler;
pub use error::DhtError;
pub use message::CompactNodeInfo;
pub use node_id::NodeId;
