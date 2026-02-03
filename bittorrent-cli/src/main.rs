use bittorrent_core::{Session, SessionConfig};
use clap::Parser;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let console_layer = console_subscriber::spawn();

    let args = Args::parse();

    let log_level = tracing::Level::from(args.log_level);

    tracing_subscriber::registry()
        .with(console_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(true)
                .with_filter(tracing_subscriber::filter::LevelFilter::from(log_level)),
        )
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

    let config = SessionConfig {
        port: args.port,
        save_path: save_dir,
        enable_dht: true,
    };

    let session = Session::new(config);

    let torrent_id = if is_magnet {
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

    println!("Added torrent: {:?}", torrent_id);
    println!("Starting download...");
    println!("Press Ctrl+C to stop");
    println!();

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;

    info!("Received shutdown signal, stopping session...");

    if let Err(e) = session.shutdown().await {
        eprintln!("Error during shutdown: {}", e);
    }

    Ok(())
}
