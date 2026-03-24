use bittorrent_core::{Session, SessionConfig, SessionEvent, utils::format_speed};
use clap::{Parser, Subcommand};
use std::time::Duration;
use tokio::time::interval;

#[derive(Parser)]
#[command(name = "torrent-rs")]
#[command(about = "BitTorrent client CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(
        global = true,
        long,
        default_value = "0.0.0.0:9000",
        help = "Prometheus metrics listen address"
    )]
    metrics_addr: String,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Download a torrent file or magnet URI")]
    Download {
        #[arg(help = "Torrent file path or magnet URI")]
        source: String,
    },

    #[command(about = "Add a torrent to the session without starting download")]
    Add {
        #[arg(help = "Torrent file path or magnet URI")]
        source: String,
    },

    #[command(about = "List active torrents")]
    List,

    #[command(about = "Show session statistics")]
    Stats,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // tracing_subscriber::fmt::init();
    console_subscriber::init();

    let cli = Cli::parse();

    let metrics_addr = cli.metrics_addr.parse().unwrap();
    cmd::metrics::install(metrics_addr).expect("Failed to install metrics exporter");

    match cli.command {
        Commands::Download { source } => download_torrent(source).await?,
        Commands::Add { source } => add_torrent(source).await?,
        Commands::List => list_torrents(),
        Commands::Stats => show_stats(),
    }

    Ok(())
}

async fn download_torrent(input: String) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{self, Write};

    let config = SessionConfig::default();
    let session = Session::new(config);

    let torrent_id = if input.starts_with("magnet:") {
        session.add_magnet(&input).await?
    } else {
        session.add_torrent(&input).await?
    };

    println!("Added torrent: {torrent_id}");

    let mut metrics_rx = session.subscribe_torrent(torrent_id).await?;
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
            _ = metrics_rx.changed() => {
                let m = metrics_rx.borrow().clone();
                let progress = if m.total_pieces > 0 {
                    f64::from(m.verified_pieces) / f64::from(m.total_pieces)
                } else {
                    0.0
                };
                #[allow(clippy::cast_sign_loss)]
                #[allow(clippy::cast_possible_truncation)]
                let download_rate = m.download_rate as u64;
                #[allow(clippy::cast_sign_loss)]
                #[allow(clippy::cast_possible_truncation)]
                let upload_rate = m.upload_rate as u64;
                print!("{:<20} | {:>8.2}% | {:>10} | {:>10} | {:>5}",
                    truncate(&m.name, 20),
                    progress * 100.0,
                    format_speed(download_rate),
                    format_speed(upload_rate),
                    m.connected_peers
                );
                io::stdout().flush().unwrap();
            }

            Ok(event) = event_rx.recv() => {
                match event {
                    SessionEvent::TorrentCompleted(id) if id == torrent_id => {
                        println!("\n\nTorrent {id} completed!");
                        break;
                    }
                    SessionEvent::TorrentError(id, err) if id == torrent_id => {
                        eprintln!("\n\nError in torrent {id}: {err}");
                        break;
                    }
                    _ => {}
                }
            }

            _ = ticker.tick() => {}
        }
    }

    println!("Shutting down session...");
    session.shutdown().await?;

    Ok(())
}

async fn add_torrent(input: String) -> Result<(), Box<dyn std::error::Error>> {
    let session = Session::new(SessionConfig::default());

    let torrent_id = if input.starts_with("magnet:") {
        session.add_magnet(&input).await?
    } else {
        session.add_torrent(&input).await?
    };

    println!("Added torrent: {torrent_id}");
    session.shutdown().await?;
    Ok(())
}

fn list_torrents() {
    println!("Listing torrents (not implemented yet)");
}

fn show_stats() {
    println!("Show stats (not implemented yet)");
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[0..max_len - 3])
    } else {
        s.to_string()
    }
}
