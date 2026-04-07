use bittorrent_core::{Session, SessionConfig, SessionEvent, utils::format_speed};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

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
        #[arg(long, help = "Continue seeding after download completes")]
        seed: bool,
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
    tracing_subscriber::fmt::init();
    // console_subscriber::init();

    let cli = Cli::parse();

    let metrics_addr = cli.metrics_addr.parse().unwrap();
    cmd::metrics::install(metrics_addr).expect("Failed to install metrics exporter");

    let config = SessionConfig::builder().enable_port_mapping(false).build();
    let session = Session::new(config);

    match cli.command {
        Commands::Download { source, seed } => download_torrent(source, session, seed).await?,
        Commands::Add { source } => add_torrent(source, session).await?,
        Commands::List => list_torrents(),
        Commands::Stats => show_stats(),
    }

    Ok(())
}

async fn download_torrent(
    input: String,
    session: Session,
    seed: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let torrent_id = if input.starts_with("magnet:") {
        session.add_magnet(&input).await?
    } else {
        session.add_torrent(&input).await?
    };

    let mut metrics_rx = session.subscribe_torrent(torrent_id).await?;
    let mut event_rx = session.subscribe();

    let progress_bar = ProgressBar::new(100);
    progress_bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] {msg} {wide_bar:.cyan/blue} ({eta})",
        )
        .unwrap()
        .progress_chars("#>-"),
    );

    let mut ticker = tokio::time::interval(Duration::from_millis(100));

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

                let progress_percent = (progress * 100.0) as u64;
                progress_bar.set_position(progress_percent);
                progress_bar.set_message(format!(
                    "{:<20} | D: {:>10}/s | U: {:>10}/s | Peers: {:>3}",
                    truncate(&m.name, 20),
                    format_speed(download_rate),
                    format_speed(upload_rate),
                    m.connected_peers
                ));
            }

            Ok(event) = event_rx.recv() => {
                match event {
                    SessionEvent::TorrentCompleted(id) if id == torrent_id => {
                        progress_bar.finish_with_message("Download completed!");
                        if !seed {
                            break;
                        }
                    }
                    SessionEvent::TorrentError(id, err) if id == torrent_id => {
                        progress_bar.finish_with_message(format!("Error: {err}"));
                        break;
                    }
                    _ => {}
                }
            }

            _ = ticker.tick() => {
                progress_bar.tick();
            }
        }
    }

    if !seed {
        session.shutdown().await?;
    }

    Ok(())
}

async fn add_torrent(input: String, session: Session) -> Result<(), Box<dyn std::error::Error>> {
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
