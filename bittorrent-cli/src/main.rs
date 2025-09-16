use bittorrent_core::{Session, TorrentStats};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;
use tracing::info;

#[derive(Parser)]
#[command(name = "bittorrent-cli")]
#[command(about = "A BitTorrent client for leeching torrents")]
#[command(long_about = "First iteration of the BitTorrent client CLI")]
struct Args {
    /// Path to the torrent file or magnet URI
    torrent: String,

    /// Listening port for incoming peer connections
    #[arg(short, long, default_value_t = 6881)]
    port: u16,

    /// Directory to save downloaded files
    #[arg(short = 'd', long)]
    save_dir: Option<PathBuf>,

    // Set log level (critical, error, warn, info, debug, trace)
    #[arg(long, value_enum, default_value_t = LogLevel::Info)]
    log_level: LogLevel,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevel> for tracing::Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => tracing::Level::ERROR,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Trace => tracing::Level::TRACE,
        }
    }
}

use std::io::{self, Write};

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    const BASE: f64 = 1024.0;

    if bytes == 0 {
        return "0 B".to_string();
    }

    let bytes_f64 = bytes as f64;
    let exponent = (bytes_f64.ln() / BASE.ln()).floor() as i32;
    let unit_index = exponent.min(UNITS.len() as i32 - 1) as usize;
    let size = bytes_f64 / BASE.powi(exponent);

    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

fn format_rate(bytes_per_second: f64) -> String {
    const UNITS: [&str; 6] = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s", "PB/s"];
    const BASE: f64 = 1024.0;

    if bytes_per_second == 0.0 {
        return "0 B/s".to_string();
    }

    let rate = bytes_per_second;
    let exponent = (rate.ln() / BASE.ln()).floor() as i32;
    let unit_index = exponent.min(UNITS.len() as i32 - 1) as usize;
    let formatted_rate = rate / BASE.powi(exponent);

    if unit_index == 0 {
        format!("{:.0} {}", formatted_rate, UNITS[0])
    } else {
        format!("{:.1} {}", formatted_rate, UNITS[unit_index])
    }
}

fn print_stats(stats: &TorrentStats) {
    let downloaded_fmt = format_size(stats.downloaded);
    let uploaded_fmt = format_size(stats.uploaded);
    let download_rate_fmt = format_rate(stats.download_rate);
    let upload_rate_fmt = format_rate(stats.upload_rate);

    let line = format!(
        "Progress: {:.1}%, dl: {} from {} peers ({}), ul: {}  ({}), ETA: {}",
        stats.progress,
        downloaded_fmt,
        stats.connected_peers,
        download_rate_fmt,
        uploaded_fmt,
        upload_rate_fmt,
        format_eta(stats)
    );

    print!("\r{:<80}", line);
    io::stdout().flush().unwrap();
}

fn format_eta(stats: &TorrentStats) -> String {
    if stats.download_rate == 0.0 || stats.progress >= 100.0 {
        return "--:--".to_string();
    }

    // Calculate remaining data based on progress (progress is already 0-100)
    let total_data = (stats.downloaded as f64 * 100.0) / stats.progress;
    let remaining_data = total_data - stats.downloaded as f64;
    let seconds_remaining = (remaining_data / stats.download_rate) as u64;

    if seconds_remaining == 0 {
        return "<1m".to_string();
    }

    let hours = seconds_remaining / 3600;
    let minutes = (seconds_remaining % 3600) / 60;
    let seconds = seconds_remaining % 60;

    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let log_level = tracing::Level::from(args.log_level);
    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(true)
        .init();

    let is_magnet = args.torrent.starts_with("magnet:");

    if !is_magnet {
        // Validate torrent file
        let torrent_path = PathBuf::from(&args.torrent);
        if !torrent_path.exists() {
            eprintln!(
                "Error: Torrent file does not exist: {}",
                torrent_path.display()
            );
            eprintln!("Please check the path and try again.");
            std::process::exit(1);
        }

        if let Some(ext) = torrent_path.extension()
            && ext != "torrent"
        {
            eprintln!("Error: File doesn't have .torrent extension");
            std::process::exit(1);
        }
    }

    // Default to $HOME/Downloads/Torrents/
    let save_dir = args.save_dir.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join("Downloads").join("Torrents")
    });

    // Ensure save directory exists
    if let Err(e) = std::fs::create_dir_all(&save_dir) {
        eprintln!(
            " Error: Failed to create save directory {}: {}",
            save_dir.display(),
            e
        );
        eprintln!("Please check permissions and try again.");
        std::process::exit(1);
    }

    info!("Torrent: {}", args.torrent);
    info!("Save directory: {}", save_dir.display());
    info!("Listening on port: {}", args.port);

    // Create session
    let mut session = Session::new(args.port, save_dir);

    // Add the torrent or magnet
    if is_magnet {
        if let Err(e) = session.add_magnet(&args.torrent) {
            eprintln!("Error adding magnet URI: {}", e);
            std::process::exit(1);
        }
    } else {
        if let Err(e) = session.add_torrent(&args.torrent) {
            eprintln!("Error adding torrent file: {}", e);
            std::process::exit(1);
        }
    }

    println!("Starting download...");
    println!("Press Ctrl+C to stop");
    println!();

    loop {
        tokio::select! {
            Some(stats) = session.stats_receiver.recv()=>{
                print_stats(&stats);
            },
            _ = tokio::signal::ctrl_c() => {
                info!(" Received shutdown signal, stopping session...");
                session.shutdown();

                tokio::time::sleep(Duration::from_millis(500)).await;
                break;
            }
        }
    }

    Ok(())
}
