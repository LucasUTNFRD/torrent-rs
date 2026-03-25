use std::net::SocketAddr;

use metrics_exporter_prometheus::PrometheusBuilder;

/// Install the Prometheus metrics exporter.
///
/// # Errors
///
/// Returns an error if the HTTP listener fails to bind to the specified address,
/// or if the Prometheus exporter fails to install.
pub fn install(addr: SocketAddr) -> anyhow::Result<()> {
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()?;
    Ok(())
}
