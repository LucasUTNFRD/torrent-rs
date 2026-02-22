use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bittorrent_common::metainfo::parse_torrent_from_file;
use bittorrent_core::{Session, SessionConfig, TorrentSummary};
use clap::Parser;
use tokio::time::interval;
use tracing_subscriber::fmt::writer::MakeWriterExt;

#[derive(Parser)]
#[command(name = "bittorrent-cli")]
#[command(about = "A lightweight command-line Bittorent client")]
#[command(version)]
struct Args {
    /// Path to the torrent file or magnet URI
    torrent: String,

    /// Listening port for incoming peer connections
    #[arg(short, long, default_value_t = 6881)]
    port: u16,

    /// Directory to save downloaded files
    #[arg(short = 'd', long)]
    save_dir: Option<PathBuf>,

    /// Directory containing files to seed (seeding mode).
    /// When provided, verifies and seeds existing content instead of downloading.
    #[arg(short = 'w', long, value_name = "CONTENT_DIR")]
    watch_dir: Option<PathBuf>,

    /// Set log level (error, warn, info, debug, trace)
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

/// Shared state for the status line
struct StatusState {
    current_line: Mutex<String>,
    needs_clear: AtomicBool,
}

impl StatusState {
    fn new() -> Self {
        Self {
            current_line: Mutex::new(String::new()),
            needs_clear: AtomicBool::new(false),
        }
    }
}

/// Custom writer that handles status line clearing
struct StatusLineWriter {
    state: Arc<StatusState>,
}

impl std::io::Write for StatusLineWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut stderr = std::io::stderr();

        // Clear the current status line before writing log message
        if self.state.needs_clear.load(Ordering::SeqCst) {
            let line = self.state.current_line.lock().unwrap();
            if !line.is_empty() {
                // Move to start of line and clear it
                write!(stderr, "\r{:<width$}\r", "", width = line.len())?;
            }
            self.state.needs_clear.store(false, Ordering::SeqCst);
        }

        stderr.write_all(buf)?;
        stderr.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

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

fn format_bytes_per_second(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}


fn get_status_line(summary: &TorrentSummary) -> String {
    let progress = summary.progress * 100.0;
    let state_str = match summary.state {
        bittorrent_core::TorrentState::FetchingMetadata => "Fetching metadata".to_string(),
        bittorrent_core::TorrentState::Checking => {
            format!("Verifying local files ({:.1}%)", progress)
        }
        bittorrent_core::TorrentState::Downloading => {
            format!(
                "Progress: {:.1}%, dl from {} of {} peers ({}), ul to {} ({})",
                progress,
                summary.peers_connected,  // connected peers
                summary.peers_discovered, // discovered peers from trackers/DHT
                format_bytes_per_second(summary.download_rate),
                0, // peers_getting_from_us - not tracked yet
                format_bytes_per_second(summary.upload_rate),
            )
        }
        bittorrent_core::TorrentState::Seeding => {
            format!(
                "Seeding, uploading to {} of {} peer(s), {}",
                0, // peers_getting_from_us - not tracked yet
                summary.peers_discovered,
                format_bytes_per_second(summary.upload_rate),
            )
        }
        bittorrent_core::TorrentState::Paused => "Paused".to_string(),
        bittorrent_core::TorrentState::Error => "Error".to_string(),
    };

    // Pad to clear previous longer lines (typical terminal width consideration)
    format!("{:<80}", state_str)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let is_magnet = args.torrent.starts_with("magnet:");
    let is_seeding = args.watch_dir.is_some();

    if is_seeding && is_magnet {
        eprintln!("Error: Seeding mode requires a .torrent file, not a magnet URI.");
        std::process::exit(1);
    }

    if !is_magnet {
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

    if let Some(ref watch_dir) = args.watch_dir {
        if !watch_dir.exists() {
            eprintln!(
                "Error: Content directory does not exist: {}",
                watch_dir.display()
            );
            std::process::exit(1);
        }
        if !watch_dir.is_dir() {
            eprintln!(
                "Error: Content path is not a directory: {}",
                watch_dir.display()
            );
            std::process::exit(1);
        }
    }

    // Default to $HOME/Downloads/Torrents/ (for download mode)
    // For seeding mode, use watch_dir as the base
    let save_dir = if let Some(ref watch_dir) = args.watch_dir {
        watch_dir.clone()
    } else {
        args.save_dir.unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join("Downloads").join("Torrents")
        })
    };

    if !is_seeding
        && let Err(e) = std::fs::create_dir_all(&save_dir)
    {
        eprintln!(
            "Error: Failed to create save directory {}: {}",
            save_dir.display(),
            e
        );
        eprintln!("Please check permissions and try again.");
        std::process::exit(1);
    }

    // Set up status line handling
    let status_state = Arc::new(StatusState::new());
    let status_state_for_writer = status_state.clone();

    let log_level = tracing::Level::from(args.log_level);

    // Create a custom layer that writes to stderr with status line handling
    let stderr_writer = move || StatusLineWriter {
        state: status_state_for_writer.clone(),
    };

    tracing_subscriber::fmt()
        .with_writer(stderr_writer.with_max_level(log_level))
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_ansi(true)
        .init();

    // Print version info like transmission-cli
    println!("bittorrent-cli 0.1.0");
    println!();

    let config = SessionConfig {
        port: args.port,
        save_path: save_dir.clone(),
        enable_dht: true,
        ..Default::default()
    };

    let session = Session::new(config);

    let torrent_id = if is_seeding {
        let torrent_info = parse_torrent_from_file(&args.torrent)?;
        let content_dir = args.watch_dir.unwrap();
        println!(
            "Verifying and seeding: {}",
            torrent_info.info.mode.name()
        );
        println!("Content directory: {}", content_dir.display());
        match session.seed_torrent(torrent_info, content_dir).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Error starting seed: {}", e);
                std::process::exit(1);
            }
        }
    } else if is_magnet {
        match session.add_magnet(&args.torrent).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Error adding magnet URI: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match session.add_torrent(&args.torrent).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Error adding torrent file: {}", e);
                std::process::exit(1);
            }
        }
    };

    // Create shutdown signal
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    let shutdown_tx_clone = shutdown_tx.clone();

    // Spawn Ctrl+C handler
    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("Failed to listen for Ctrl+C: {}", e);
        }
        let _ = shutdown_tx_clone.send(()).await;
    });

    // Status line updater
    let status_state_for_updater = status_state.clone();
    let mut status_ticker = interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            _ = status_ticker.tick() => {
                // Get torrent info and update status line
                match session.get_torrent(torrent_id).await {
                    Ok(Some(summary)) => {
                        let line = get_status_line(&summary);

                        // Update shared state
                        {
                            let mut current = status_state_for_updater.current_line.lock().unwrap();
                            *current = line.clone();
                        }
                        status_state_for_updater.needs_clear.store(true, Ordering::SeqCst);

                        // Print status line (overwrites previous)
                        print!("\r{}", line);
                        std::io::stdout().flush()?;
                    }
                    Ok(None) => {
                        // Torrent not found (shouldn't happen normally)
                    }
                    Err(e) => {
                        eprintln!("\nError getting torrent stats: {}", e);
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                println!("\nStopping torrent...");

                if let Err(e) = session.shutdown().await {
                    eprintln!("Error during shutdown: {}", e);
                }
                break;
            }
        }
    }

    Ok(())
}
