use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use bittorrent_core::Session;
use tokio::time::Instant;

const DEFAULT_PORT: u16 = 6881;
const PATH: &str = "$HOME/Downloads/Torrents/";

#[derive(Debug)]
pub struct TorrentMetrics {
    pub start_time: Instant,
    pub bytes_downloaded: AtomicU64,
    pub bytes_uploaded: AtomicU64,
    pub pieces_completed: AtomicU32,
    pub peer_connections_total: AtomicU32,
    pub peer_connections_active: AtomicU32,
    pub peer_disconnections: AtomicU32,
    pub piece_requests_sent: AtomicU64,
    pub piece_requests_received: AtomicU64,
    pub handshake_failures: AtomicU32,
    pub piece_verification_failures: AtomicU32,
}

impl TorrentMetrics {
    pub fn download_rate(&self) -> f64 {
        let bytes = self.bytes_downloaded.load(Ordering::Relaxed) as f64;
        let seconds = self.start_time.elapsed().as_secs_f64();
        if seconds > 0.0 { bytes / seconds } else { 0.0 }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging

    tracing_subscriber::fmt::init();

    tracing::info!("Starting BitTorrent client...");

    let session = Session::new(DEFAULT_PORT, PATH.into());
    tracing::info!("Save directory: {:?}", PATH);
    tracing::info!("Listening on port: {}", DEFAULT_PORT);

    session.add_torrent("sample_torrents/debian-13.0.0-amd64-netinst.iso.torrent");

    tracing::info!("Session running. Press Ctrl+C to shutdown.");

    // Handle graceful shutdown
    tokio::select! {
        _ = session.handle => {
            tracing::info!("Session completed normally");
        }
        _ = tokio::signal::ctrl_c() => {
            // session.
            tracing::info!("Received shutdown signal, stopping session...");
        }
    }

    Ok(())
}
