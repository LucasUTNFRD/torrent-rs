use std::env;
use std::io::Write;
use std::time::Duration;

use bittorrent_core::{Session, SessionConfig, SessionEvent, TorrentState, TorrentSummary};
use tokio::time::interval;

const CACHY_OS_MAGNET_LINK: &str = "magnet:?xt=urn:btih:325a8dd4a4b6164bf8fca28cf47d5d05288229ee&dn=cachyos-desktop-linux-260124.iso&tr=udp%3A%2F%2Ffosstorrents.com%3A6969%2Fannounce&tr=http%3A%2F%2Ffosstorrents.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&tr=udp%3A%2F%2Ftracker-udp.gbitt.info%3A80%2Fannounce&tr=udp%3A%2F%2Fopen.demonii.com%3A1337%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.theoks.net%3A6969%2Fannounce&tr=udp%3A%2F%2Fopentracker.io%3A6969%2Fannounce";

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{:.2} {}", size, UNITS[unit_index])
    }
}

fn format_speed(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

fn get_status_line(summary: &TorrentSummary) -> String {
    let progress = summary.progress * 100.0;
    let line = match summary.state {
        TorrentState::FetchingMetadata => "Fetching metadata...".to_string(),
        TorrentState::Checking => format!("Verifying local files ({progress:.1}%)"),
        TorrentState::Downloading => {
            format!(
                "Progress: {:.1}%, dl from {} of {} peers ({}), ul to {} ({})",
                progress,
                summary.peers_connected,
                summary.peers_discovered,
                format_speed(summary.download_rate),
                0, // peers_getting_from_us - not tracked yet
                format_speed(summary.upload_rate),
            )
        }
        TorrentState::Seeding => {
            format!(
                "Seeding, uploading to {} of {} peer(s), {}",
                0, // peers_getting_from_us - not tracked yet
                summary.peers_discovered,
                format_speed(summary.upload_rate),
            )
        }
        TorrentState::Paused => "Paused".to_string(),
        TorrentState::Error => "Error".to_string(),
    };

    format!("{line:<80}")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let config = SessionConfig::default();
    let session = Session::new(config);

    let torrent_id = session.add_magnet(CACHY_OS_MAGNET_LINK).await?;
    println!("Added torrent: {torrent_id}");

    // Subscribe to session events for completion notification
    let mut events = session.subscribe();

    // Poll progress every 500ms, stop when the torrent completes
    let mut status_ticker = interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            _ = status_ticker.tick() => {
                if let Ok(Some(summary)) = session.get_torrent(torrent_id).await {
                    print!("\r{}", get_status_line(&summary));
                    std::io::stdout().flush()?;
                }
            }
            event = events.recv() => {
                match event {
                    Ok(SessionEvent::TorrentCompleted(id)) if id == torrent_id => {
                        println!("\nDownload complete!");
                        break;
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        eprintln!("\nSession closed before download completed");
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("Missed {n} events, continuing...");
                        continue;
                    }
                }
            }
        }
    }

    session.shutdown().await?;
    Ok(())
}
