//! BitTorrent Daemon
//!
//! A headless BitTorrent client that runs as a foreground service.
//! Designed for server/CLI usage with future IPC support.

use std::path::PathBuf;
use std::sync::Arc;

use bittorrent_core::{Session, SessionConfig};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
#[command(name = "btd")]
#[command(about = "BitTorrent daemon - headless torrent client")]
#[command(version)]
struct Args {
    /// Listening port for incoming peer connections
    #[arg(short, long, default_value_t = 6881)]
    port: u16,

    /// Port for the IPC server
    #[arg(long, default_value_t = 6969)]
    ipc_port: u16,

    /// Directory to save downloaded files
    #[arg(short = 'd', long)]
    save_dir: Option<PathBuf>,

    /// Log level (error, warn, info, debug, trace)
    #[arg(long, value_enum, default_value_t = LogLevel::Info)]
    log_level: LogLevel,

    /// Disable DHT (distributed hash table)
    #[arg(long)]
    no_dht: bool,

    /// Torrent files or magnet URIs to add on startup
    #[arg(trailing_var_arg = true)]
    torrents: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum IpcCommand {
    AddTorrent { path: String },
    AddMagnet { uri: String },
    ListTorrents,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum IpcResponse {
    Success { message: String },
    Error { message: String },
    TorrentList { torrents: Vec<String> },
}

async fn run_ipc_server(session: Arc<Session>, port: u16) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind IPC server to {}: {}", addr, e);
            return;
        }
    };

    info!("IPC server listening on {}", addr);

    loop {
        let (mut socket, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("IPC accept error: {}", e);
                continue;
            }
        };

        let session = session.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Err(e) = socket.read_to_end(&mut buf).await {
                warn!("IPC read error: {}", e);
                return;
            }

            let cmd: IpcCommand = match serde_json::from_slice(&buf) {
                Ok(c) => c,
                Err(e) => {
                    let resp = IpcResponse::Error {
                        message: format!("Invalid command: {}", e),
                    };
                    let _ = socket.write_all(&serde_json::to_vec(&resp).unwrap()).await;
                    return;
                }
            };

            let response = match cmd {
                IpcCommand::AddTorrent { path } => match session.add_torrent(&path).await {
                    Ok(id) => IpcResponse::Success {
                        message: format!("Added torrent: {:?}", id),
                    },
                    Err(e) => IpcResponse::Error {
                        message: format!("Failed to add torrent: {}", e),
                    },
                },
                IpcCommand::AddMagnet { uri } => match session.add_magnet(&uri).await {
                    Ok(id) => IpcResponse::Success {
                        message: format!("Added magnet: {:?}", id),
                    },
                    Err(e) => IpcResponse::Error {
                        message: format!("Failed to add magnet: {}", e),
                    },
                },
                IpcCommand::ListTorrents => match session.list_torrents().await {
                    Ok(list) => IpcResponse::TorrentList {
                        torrents: list.into_iter().map(|t| t.name).collect(),
                    },
                    Err(e) => IpcResponse::Error {
                        message: format!("Failed to list torrents: {}", e),
                    },
                },
            };

            let _ = socket
                .write_all(&serde_json::to_vec(&response).unwrap())
                .await;
        });
    }
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevel> for tracing_subscriber::filter::LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Self::ERROR,
            LogLevel::Warn => Self::WARN,
            LogLevel::Info => Self::INFO,
            LogLevel::Debug => Self::DEBUG,
            LogLevel::Trace => Self::TRACE,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Initialize tracing
    let log_level = tracing_subscriber::filter::LevelFilter::from(args.log_level);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_thread_ids(false)
                .with_file(true)
                .with_line_number(true)
                .with_filter(log_level),
        )
        .init();

    // Resolve save directory
    let save_dir = args.save_dir.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join("Downloads").join("Torrents")
    });

    // Ensure save directory exists
    if let Err(e) = std::fs::create_dir_all(&save_dir) {
        eprintln!(
            "Error: Failed to create save directory {}: {}",
            save_dir.display(),
            e
        );
        std::process::exit(1);
    }

    info!("BitTorrent daemon starting");
    info!("  Peer port: {}", args.port);
    info!("  Save directory: {}", save_dir.display());
    info!(
        "  DHT: {}",
        if args.no_dht { "disabled" } else { "enabled" }
    );

    // Create session configuration
    let config = SessionConfig {
        port: args.port,
        save_path: save_dir,
        enable_dht: !args.no_dht,
        ..Default::default()
    };

    // Create and start session
    let session = Arc::new(Session::new(config));

    // Start IPC server
    let ipc_session = session.clone();
    let ipc_port = args.ipc_port;
    tokio::spawn(async move {
        run_ipc_server(ipc_session, ipc_port).await;
    });

    // Add any torrents specified on command line
    for torrent in &args.torrents {
        let is_magnet = torrent.starts_with("magnet:");

        let result = if is_magnet {
            session.add_magnet(torrent).await
        } else {
            let path = PathBuf::from(torrent);
            if !path.exists() {
                warn!("Torrent file does not exist: {}", path.display());
                continue;
            }
            session.add_torrent(&path).await
        };

        match result {
            Ok(id) => info!("Added torrent: {:?}", id),
            Err(e) => warn!("Failed to add torrent {}: {}", torrent, e),
        }
    }

    info!("Daemon ready. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT, shutting down...");
        }
        _ = sigterm() => {
            info!("Received SIGTERM, shutting down...");
        }
    }

    // Graceful shutdown
    if let Err(e) = session.shutdown().await {
        warn!("Error during shutdown: {}", e);
    }

    info!("Daemon stopped");
    Ok(())
}

/// Wait for SIGTERM signal (Unix only)
#[cfg(unix)]
async fn sigterm() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
    sigterm.recv().await;
}

/// No SIGTERM on non-Unix platforms, just wait forever
#[cfg(not(unix))]
async fn sigterm() {
    std::future::pending::<()>().await
}
