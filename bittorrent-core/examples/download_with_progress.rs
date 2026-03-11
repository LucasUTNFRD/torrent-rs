use bittorrent_core::{Session, SessionConfig, types::SessionEvent, utils::format_speed};
use std::{env, time::Duration};
use tokio::time::interval;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <torrent-file-or-magnet>", args[0]);
        return Ok(());
    }

    let input = &args[1];

    // 1. Setup Session
    let config = SessionConfig::default();
    let session = Session::new(config);

    // 2. Add Torrent
    let torrent_id = if input.starts_with("magnet:") {
        session.add_magnet(input).await?
    } else {
        session.add_torrent(input).await?
    };

    println!("Added torrent: {}", torrent_id);

    // 3. Subscribe to metrics for this torrent
    let mut metrics_rx = session.subscribe_torrent(torrent_id).await?;

    // 4. Also subscribe to global events to know when it finishes
    let mut event_rx = session.subscribe();

    println!("Starting download loop...");
    println!(
        "{:<20} | {:<10} | {:<10} | {:<10} | {:<10}",
        "Name", "Progress", "Down", "Up", "Peers"
    );
    println!("{}", "-".repeat(70));

    let mut ticker = interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            // Watch for metrics changes
            _ = metrics_rx.changed() => {
                let m = metrics_rx.borrow().clone();
                // We use \r to overwrite the line for a simple CLI progress bar
                print!("\r{:<20} | {:>8.2}% | {:>10} | {:>10} | {:>5}",
                    truncate(&m.name, 20),
                    m.progress * 100.0,
                    format_speed(m.download_rate),
                    format_speed(m.upload_rate),
                    m.peers_connected
                );
                use std::io::{self, Write};
                io::stdout().flush().unwrap();
            }

            // Watch for completion event
            Ok(event) = event_rx.recv() => {
                match event {
                    SessionEvent::TorrentCompleted(id) if id == torrent_id => {
                        println!("\n\nTorrent {} completed!", id);
                        break;
                    }
                    SessionEvent::TorrentError(id, err) if id == torrent_id => {
                        eprintln!("\n\nError in torrent {}: {}", id, err);
                        break;
                    }
                    _ => {}
                }
            }

            // Optional: fallback ticker if no metrics change frequently
            _ = ticker.tick() => {}
        }
    }

    println!("Shutting down session...");
    session.shutdown().await?;

    Ok(())
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[0..max_len - 3])
    } else {
        s.to_string()
    }
}
