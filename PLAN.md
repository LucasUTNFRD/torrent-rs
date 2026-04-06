# UPnP Implementation Plan Using igd_next

## Executive Summary

Replace the current `portmapper` crate with `igd_next` for UPnP port mapping. The current implementation fails due to a 1-second probe timeout that is insufficient for many routers, including the one in this environment. Libtorrent successfully uses longer timeouts (2-24+ seconds), and `igd_next` provides a 5-second timeout by default.

## Problem Analysis

### Current State
- **Library**: `portmapper` v0.15.0
- **Issue**: Probe timeout is hardcoded to 1 second
- **Symptom**: UPnP detection fails even though router supports UPnP
- **Evidence**:
  - `upnpc -l` successfully discovers UPnP gateway
  - `upnpc -a` successfully creates port mapping
  - Libtorrent works (uses exponential backoff: 2s, 4s, 6s...)
  - Portmapper probe fails after 1 second

### Root Cause
```rust
// portmapper/src/defaults.rs (internal, not configurable)
pub(crate) const UPNP_SEARCH_TIMEOUT: Duration = Duration::from_secs(1);
```

The probe gives up too quickly, before the router has time to respond.

### Why igd_next is Better
1. **Longer timeout**: 5-second default (vs 1 second)
2. **Direct operations**: No separate probe step
3. **Clearer errors**: Returns `igd_next::Result` with specific error types
4. **Built-in renewal**: Lease management with background task
5. **Clean cleanup**: Automatic port unmapping on Drop

## Implementation Plan

### Phase 1: Dependency Setup

#### Step 1.1: Add igd_next Dependency
**File**: `bittorrent-core/Cargo.toml`

```toml
[dependencies]
# Remove or comment out portmapper (keep for potential rollback)
# portmapper = "0.15.0"
igd_next = "0.15"  # UPnP gateway discovery and port mapping
```

#### Step 1.2: Create Port Mapping Module
**File**: `bittorrent-core/src/port_mapping.rs` (new file)

This module will:
- Wrap `igd_next` functionality
- Provide async port mapping operations
- Handle renewal background task
- Manage lifecycle (create/renew/remove)

### Phase 2: Configuration Changes

#### Step 2.1: Update SessionConfig
**File**: `bittorrent-core/src/session_config.rs`

```rust
// Replace separate UPnP/NAT-PMP flags with unified config
pub struct SessionConfig {
    // OLD:
    // pub enable_upnp: bool,
    // pub enable_natpmp: bool,
    
    // NEW:
    /// Enable UPnP port mapping using igd_next
    /// Attempts to automatically create port forwarding on the router
    pub enable_port_mapping: bool,
    
    // ... other fields
}

impl SessionConfigBuilder {
    // OLD:
    // pub fn enable_upnp(mut self, enabled: bool) -> Self
    // pub fn enable_natpmp(mut self, enabled: bool) -> Self
    
    // NEW:
    pub fn enable_port_mapping(mut self, enabled: bool) -> Self {
        self.enable_port_mapping = enabled;
        self
    }
}
```

#### Step 2.2: Update Binary
**File**: `cmd/src/main.rs`

```rust
let config = SessionConfig::builder()
    .enable_port_mapping(true)  // Enable UPnP
    .build();
```

### Phase 3: Port Mapping Implementation

#### Step 3.1: Create PortOpener Wrapper
**File**: `bittorrent-core/src/port_mapping.rs`

```rust
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use igd_next::{aio, PortMappingProtocol, SearchOptions, Gateway};
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
    /// * `internal_port` - Local port toforward
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
        // Discover UPnP gateway with 5-second timeout
        info!("Searching for UPnP gateway...");
        let gateway = aio::tokio::search_gateway(SearchOptions {
            timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        }).await?;
        
        // Get local IP address
        let internal_ip = get_local_ipv4()
            .ok_or_else(|| igd_next::Error::NoGatewayAvailable)?;
        let internal_addr = SocketAddrV4::new(internal_ip, internal_port);
        
        // Get external (public) IP
        let public_ip = gateway.get_external_ip().await?;
        
        // Create port mapping
        let external_port = if let Some(desired_port) = desired_external_port {
            gateway.add_port(
                protocol,
                desired_port,
                internal_addr,
                Self::LEASE_DURATION.as_secs() as u32,
                "torrent-rs",
            ).await?;
            desired_port
        } else {
            gateway.add_any_port(
                protocol,
                internal_addr,
                Self::LEASE_DURATION.as_secs() as u32,
                "torrent-rs",
            ).await?
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
                match self.gateway.add_port(
                    self.protocol,
                    self.external_addr.port(),
                    self.internal_addr,
                    self.lease_duration.as_secs() as u32,
                    "torrent-rs",
                ).await {
                    Ok(()) => {
                        info!(
                            "UPnP port mapping renewed: {}",
                            self.external_addr
                        );
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
        
        match blocking_gateway.remove_port(self.protocol, self.external_addr.port()) {
            Ok(()) => info!("UPnP port mapping removed: {}", self.external_addr),
            Err(e) => warn!("Failed to remove UPnP mapping: {}", e),
        }
    }
}

/// Get local IPv4 address for UPnP
fn get_local_ipv4() -> Option<Ipv4Addr> {
    // Try netdev first
    if let Some(gateway) = netdev::get_default_gateway().ok() {
        if let Some(ip) = gateway.ipv4.iter().next() {
            return Some(*ip);
        }
    }
    
    // Fallback: iterate network interfaces
    for iface in netdev::interface::get_interfaces() {
        // Skip loopback and docker interfaces
        if iface.name.starts_with("lo") || iface.name.starts_with("docker") || iface.name.starts_with("br-") {
            continue;
        }
        
        // Find first non-local IPv4
        for addr in iface.ipv4 {
            let ip = addr.addr();
            if !ip.is_loopback() && !ip.is_link_local() {
                return Some(ip);
            }
        }
    }
    
    None
}
```

#### Step 3.2: Update SessionManager
**File**: `bittorrent-core/src/session.rs`

```rust
use crate::port_mapping::PortMapping;

pub struct SessionManager {
    // ... existing fields ...
    
    // OLD:
    // portmapper: Option<portmapper::Client>,
    
    // NEW:
    port_mapping: Option<JoinHandle<()>>,  // Renewal task handle
}

impl SessionManager {
    pub async fn start(mut self) {
        // Bind TCP listener first
        let listener = TcpListener::bind(self.config.listen_addr())
            .await
            .expect("Failed to bind TCP listener");
        
        tracing::info!("Bound to {:?}", listener.local_addr());
        
        // Attempt UPnP port mapping
        let renewal_handle = if self.config.enable_port_mapping {
            self.setup_port_mapping().await
        } else {
            tracing::info!("Port mapping disabled by configuration");
            None
        };
        
        self.port_mapping = renewal_handle;
        
        // ... rest of startup code ...
    }
    
    async fn setup_port_mapping(&self) -> Option<JoinHandle<()>> {
        use igd_next::PortMappingProtocol;
        
        tracing::info!("Attempting UPnP port mapping...");
        
        let local_port = self.config.listen_addr().port();
        
        match PortMapping::new(
            local_port,
            Some(local_port),  // Try to keep same port externally
            PortMappingProtocol::TCP,
        ).await {
            Ok(mapping) => {
                let external_addr = mapping.external_addr();
                tracing::info!(
                    "UPnP successful: external {} -> internal {}",
                    external_addr,
                    local_port
                );
                
                // Start renewal task
                Some(mapping.spawn_renewal_task())
            }
            Err(e) => {
                tracing::warn!(
                    "UPnP port mapping failed ({}). Continuing in outbound-only mode.",
                    e
                );
                None
            }
        }
    }
    
    async fn handle_shutdown(&mut self) -> Result<(), SessionError> {
        // Cancel port mapping renewal task
        if let Some(handle) = self.port_mapping.take() {
            handle.abort();
            // PortMapping Drop will remove the port mapping
        }
        
        // ... existing shutdown code ...
    }
}
```

### Phase 4: Error Handling Improvements

#### Step 4.1: Add Diagnostic Logging
**File**: `bittorrent-core/src/port_mapping.rs`

Add detailed logging for debugging:

```rust
pub async fn new(...) -> igd_next::Result<Self> {
    info!("Starting UPnP discovery...");
    
    // Log network interface info
    if let Some(ip) = get_local_ipv4() {
        info!("Local IP for UPnP: {}", ip);
    } else {
        warn!("No suitable local IPv4 address found");
    }
    
    // Discover gateway
    let gateway = match aio::tokio::search_gateway(SearchOptions {
        timeout: Some(Duration::from_secs(5)),
        ..Default::default()
    }).await {
        Ok(gw) => {
            info!("UPnP gateway discovered at: {}", gw.addr);
            gw
        }
        Err(e) => {
            warn!("UPnP gateway discovery failed: {}", e);
            info!("Troubleshooting: Ensure router has UPnP enabled and firewall allows UDP port 1900");
            return Err(e);
        }
    };
    
    // ... rest of implementation ...
}
```

#### Step 4.2: Specific Error Messages
Map `igd_next::Error` to user-friendly messages:

```rust
Err(e) => {
    let message = match e {
        igd_next::Error::NoGatewayAvailable => 
            "No UPnP gateway found. Is your router UPnP-enabled?",
        igd_next::Error::NoPortMappingAvailable => 
            "Router rejected port mapping. Try a different port or check router settings.",
        igd_next::Error::PortMappingConflict => 
            "Port already mapped by another device. Choose a different port.",
        _ => format!("UPnP error: {}", e),
    };
    tracing::warn!("{}", message);
    None
}
```

### Phase 5: Testing

#### Step 5.1: Unit Tests
**File**: `bittorrent-core/src/port_mapping.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    // Note: These tests require a UPnP-enabled router
    // They should be marked #[ignore] for CI
    
    #[ignore]
    #[tokio::test]
    async fn test_upnp_discovery() {
        let result = PortMapping::new(
            6881,
            Some(6881),
            PortMappingProtocol::TCP,
        ).await;
        
        match result {
            Ok(mapping) => {
                println!("External address: {}", mapping.external_addr());
                // Test cleanup
                drop(mapping);
            }
            Err(e) => {
                println!("UPnP not available (expected in CI): {}", e);
            }
        }
    }
}
```

#### Step 5.2: Manual Testing Checklist
- [ ] Verify UPnP creates port mapping on router
- [ ] Verify external IP is detected correctly
- [ ] Verify renewal task runs every 225 seconds (75% of 300)
- [ ] Verify port mapping is removed on clean shutdown
- [ ] Verify graceful degradation when UPnP unavailable
- [ ] Test with invalid local IP
- [ ] Test with firewall blocking UDP 1900
- [ ] Compare with `upnpc -l` output

### Phase 6: Documentation

#### Step 6.1: Update Module Documentation
**File**: `bittorrent-core/src/port_mapping.rs`

```rust
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
```

#### Step 6.2: Update User-Facing Docs
**File**: `README.md` (if exists)

```markdown
## UPnP Port Mapping

The client supports automatic port mapping via UPnP for NAT traversal.

### Enabling UPnP

```rust
let config = SessionConfig::builder()
    .enable_port_mapping(true)
    .build();
```

### Requirements

- Router must have UPnP enabled
- Firewall must allow UDP port 1900 (multicast)
- Local network must support multicast

### Troubleshooting

If UPnP fails, the client will operate in outbound-only mode (slower peer discovery).

**Check UPnP availability:**
```bash
# Linux
upnpc -l

# Should show: "Found valid IGD" and external IP
```

**Manual verification:**
```bash
# Add mapping
upnpc -a 192.168.1.47 6881 6881 TCP

# Remove mapping
upnpc -d 6881 TCP
```


## Success Criteria

### Must Have
- [ ] UPnP discovery succeeds within 5 seconds
- [ ] Port mapping created when UPnP available
- [ ] External IP reported correctly
- [ ] Graceful degradation when UPnP unavailable
- [ ] Clean shutdown removes port mapping

### Should Have
- [ ] Lease renewal works automatically
- [ ] Multiple local IPs handled correctly
- [ ] Error messages are actionable
- [ ] Works on Linux, macOS, Windows

### Nice to Have
- [ ] Metrics: UPnP success rate, latency
- [ ] Fallback to NAT-PMP/PCP (future)
- [ ] Configuration for timeout values
- [ ] Support for UDP port mapping

## Timeline Estimate

| Phase | Duration | Dependencies |
|-------|----------|--------------|
| 1. Dependency Setup | 15 min | None |
| 2. Configuration Changes | 30 min | Phase 1 |
| 3. Port Mapping Impl | 2 hours | Phase 2 |
| 4. Error Handling | 1 hour | Phase 3 |
| 5. Testing | 1-2 hours | Phase 4 |
| 6. Documentation | 30 min | Phase 3-5 |
| 7. Migration | 30 min | Phase 1-6 |
| **Total** | **5-6 hours** | |

## Risk Assessment

### High Risk
- **UPnP discovery still fails**: Mitigate by adding longer timeout configuration
- **Router incompatibility**: Mitigate by testing on multiple router brands

### Medium Risk
- **Lease renewal fails**: Mitigate by logging and attempting rediscovery
- **Cleanup doesn't run**: Mitigate by using Drop trait guarantee

### Low Risk
- **Duplicate ports on restart**: Router handles duplicate mapping requests gracefully

## References

- **igd_next docs**: https://docs.rs/igd_next/
- **Portmapper repo**: https://github.com/n0-computer/net-tools
- **UPnP spec**: https://openconnectivity.org/developer/specifications/upnp-resources/upnp/
- **Libtorrent UPnP**: https://github.com/arvidn/libtorrent/blob/master/src/upnp.cpp
