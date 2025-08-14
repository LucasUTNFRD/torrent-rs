//! BitTorrent tracker client library
//!
//! This crate provides HTTP and UDP tracker client implementations
//! for BitTorrent applications.

pub use client::{TrackerHandler, TrackerManager, TrackerMessage};
pub use error::TrackerError;

mod client;
mod error;
mod http;
mod types;
mod udp;

pub use url::Url;

pub use http::HttpTrackerClient;
pub use types::{AnnounceParams, Events, TrackerResponse};
pub use udp::UdpTrackerClient;

pub use client::TrackerClient;
