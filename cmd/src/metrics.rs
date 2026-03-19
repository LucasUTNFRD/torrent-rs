use std::net::SocketAddr;

use metrics_exporter_prometheus::PrometheusBuilder;

pub fn install(addr: SocketAddr) -> anyhow::Result<()> {
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()?;
    Ok(())
}
