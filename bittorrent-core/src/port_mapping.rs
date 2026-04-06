//! UPnP port mapping using igd_next
//!
//! This module provides automatic port forwarding setup for BitTorrent clients
//! using UPnP (Universal Plug and Play) via the `igd_next` crate.
//!
//! # Architecture
//!
//! 1. **Discovery**: Searches for UPnP-enabled gateway (5-second timeout)
//! 2. **Mapping**: Creates TCP port forwarding rule on the gateway
//! 3. **Renewal**: Background task refreshes mapping before lease expires
//! 4. **Cleanup**: Automatically removes mapping on drop
//!
//! # Usage
//!
//! ```rust,no_run
//! use bittorrent_core::port_mapping::PortMapping;
//! use igd_next::PortMappingProtocol;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create port mapping
//!     let mapping = PortMapping::new(
//!         6881,                    // Internal port
//!         Some(6881),              // Desired external port (None = let router choose)
//!         PortMappingProtocol::TCP,
//!     ).await?;
//!     
//!     println!("External address: {}", mapping.external_addr());
//!     
//!     // Start renewal background task
//!     let renewal_handle = mapping.spawn_renewal_task();
//!     
//!     // Application runs...
//!     
//!     // On shutdown, abort renewal and remove mapping
//!     renewal_handle.abort();
//!     Ok(())
//! }
//! ```
//!
//! # Troubleshooting
//!
//! If UPnP fails:
//! - Ensure router has UPnP enabled in admin panel
//! - Check firewall allows UDP port 1900 (multicast)
//! - Verify not running in Docker/port-forwarding environment
//! - Try manual mapping with `upnpc -a <local-ip> 6881 6881 TCP`

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use igd_next::{PortMappingProtocol, SearchOptions, aio};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// Manages UPnP port mapping lifecycle
pub struct PortMapping {
    gateway: aio::Gateway<aio::tokio::Tokio>,
    internal_addr: SocketAddrV4,
    external_addr: SocketAddr,
    protocol: PortMappingProtocol,
    lease_duration: Duration,
}

impl PortMapping {
    /// Lease duration for port mappings (5 minutes)
    const LEASE_DURATION: Duration = Duration::from_secs(300);

    /// Attempt to create UPnP port mapping
    ///
    /// # Arguments
    /// * `internal_port` - Local port to forward
    /// * `desired_external_port` - Preferred external port (None = let router choose)
    /// * `protocol` - TCP or UDP
    ///
    /// # Returns
    /// * `Ok(PortMapping)` - Successfully created mapping
    /// * `Err(...)` - UPnP failed or unavailable
    pub async fn new(
        internal_port: u16,
        desired_external_port: Option<u16>,
        protocol: PortMappingProtocol,
    ) -> igd_next::Result<Self> {
        // Get local IP address FIRST (before discovery)
        let internal_ip = get_local_ipv4();
        let internal_ip = match internal_ip {
            Some(ip) => {
                info!("Local IP for UPnP: {}", ip);
                ip
            }
            None => {
                warn!("No suitable local IPv4 address found for UPnP");
                return Err(igd_next::Error::SearchError(
                    igd_next::SearchError::IoError(std::io::Error::new(
                        std::io::ErrorKind::AddrNotAvailable,
                        "No suitable local IPv4 address found",
                    )),
                ));
            }
        };

        // Discover UPnP gateway with 30-second timeout
        // Using longer timeout as some routers are slow to respond to SSDP discovery
        info!("Searching for UPnP gateway on interface {}...", internal_ip);
        let bind_addr = SocketAddr::new(std::net::IpAddr::V4(internal_ip), 0);
        let gateway = match aio::tokio::search_gateway(SearchOptions {
            bind_addr,
            timeout: Some(Duration::from_secs(30)),
            ..Default::default()
        })
        .await
        {
            Ok(gw) => {
                info!("UPnP gateway discovered at: {}", gw.addr);
                gw
            }
            Err(e) => {
                warn!("UPnP discovery failed: {} (bind_addr={})", e, bind_addr);
                warn!(
                    "Troubleshooting: Ensure router has UPnP enabled and firewall allows UDP port 1900"
                );
                return Err(igd_next::Error::SearchError(e));
            }
        };

        let internal_addr = SocketAddrV4::new(internal_ip, internal_port);

        // Get external (public) IP
        let public_ip = gateway.get_external_ip().await?;

        // Create port mapping
        let external_port = if let Some(desired_port) = desired_external_port {
            gateway
                .add_port(
                    protocol,
                    desired_port,
                    SocketAddr::V4(internal_addr),
                    Self::LEASE_DURATION.as_secs() as u32,
                    "torrent-rs",
                )
                .await?;
            desired_port
        } else {
            gateway
                .add_any_port(
                    protocol,
                    SocketAddr::V4(internal_addr),
                    Self::LEASE_DURATION.as_secs() as u32,
                    "torrent-rs",
                )
                .await?
        };

        let external_addr = SocketAddr::new(public_ip, external_port);

        info!(
            "UPnP port mapping created: {}:{} -> {}",
            public_ip, external_port, internal_addr
        );

        Ok(Self {
            gateway,
            internal_addr,
            external_addr,
            protocol,
            lease_duration: Self::LEASE_DURATION,
        })
    }

    /// Get the external (public) address
    pub fn external_addr(&self) -> SocketAddr {
        self.external_addr
    }

    /// Spawn a background task to continuously renew the port mapping
    ///
    /// Returns a JoinHandle that can be aborted to stop renewal
    pub fn spawn_renewal_task(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(self.lease_duration * 3 / 4).await;

                // Renew before expiration
                match self
                    .gateway
                    .add_port(
                        self.protocol,
                        self.external_addr.port(),
                        SocketAddr::V4(self.internal_addr),
                        self.lease_duration.as_secs() as u32,
                        "torrent-rs",
                    )
                    .await
                {
                    Ok(()) => {
                        info!("UPnP port mapping renewed: {}", self.external_addr);
                    }
                    Err(e) => {
                        warn!("UPnP renewal failed, attempting rediscovery: {}", e);
                        // Could attempt to rediscover gateway here
                        break;
                    }
                }
            }
        })
    }
}

impl Drop for PortMapping {
    fn drop(&mut self) {
        // Use blocking API for cleanup
        let blocking_gateway = igd_next::Gateway {
            addr: self.gateway.addr,
            root_url: std::mem::take(&mut self.gateway.root_url),
            control_url: std::mem::take(&mut self.gateway.control_url),
            control_schema_url: std::mem::take(&mut self.gateway.control_schema_url),
            control_schema: std::mem::take(&mut self.gateway.control_schema),
        };

        let protocol = self.protocol;
        let port = self.external_addr.port();
        let external_addr = self.external_addr;
        tokio::task::spawn_blocking(move || match blocking_gateway.remove_port(protocol, port) {
            Ok(()) => info!("UPnP port mapping removed: {}", external_addr),
            Err(e) => warn!("Failed to remove UPnP mapping: {}", e),
        });
    }
}

/// Get local IPv4 address for UPnP
fn get_local_ipv4() -> Option<Ipv4Addr> {
    // Connect a UDP socket to an external address — no packet is sent,
    // but the kernel selects the correct source interface and fills in
    // the local address. This is the only correct way to get the
    // routable source IP without parsing routing tables or spawning processes.
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let local_addr = socket.local_addr().ok()?;

    match local_addr.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_link_local() => {
            info!("Local IP for UPnP: {}", ip);
            Some(ip)
        }
        _ => {
            warn!("No suitable local IPv4 address found for UPnP");
            None
        }
    }
}
