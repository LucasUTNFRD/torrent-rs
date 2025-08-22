use bittorrent_core::Session;

const DEFAULT_PORT: u16 = 6881;
const PATH: &str = "$HOME/Downloads/Torrents/";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    tracing::info!("Starting BitTorrent client...");

    let session = Session::new(DEFAULT_PORT, PATH.into());
    tracing::info!("Save directory: {:?}", PATH);
    tracing::info!("Listening on port: {}", DEFAULT_PORT);

    session.add_torrent("sample_torrents/sample.torrent");

    tracing::info!("Session running. Press Ctrl+C to shutdown.");

    // Handle graceful shutdown
    tokio::select! {
        _ = session.handle => {
            tracing::info!("Session completed normally");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received shutdown signal, stopping session...");
        }
    }

    Ok(())
}
