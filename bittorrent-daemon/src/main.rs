//! BitTorrent Daemon
//!
//! A headless BitTorrent client that runs as a foreground service.
//! Designed for server/CLI usage with HTTP API support.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use bittorrent_common::types::InfoHash;
use bittorrent_core::{Session, SessionConfig};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};
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

    /// Port for the HTTP API server
    #[arg(long, default_value_t = 6969)]
    api_port: u16,

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
struct AddTorrentRequest {
    path: Option<String>,
    uri: Option<String>,
}

#[derive(Serialize)]
struct ApiError {
    error: String,
}

async fn list_torrents(State(session): State<Arc<Session>>) -> impl IntoResponse {
    match session.list_torrents().await {
        Ok(torrents) => (StatusCode::OK, Json(torrents)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn add_torrent(
    State(session): State<Arc<Session>>,
    Json(payload): Json<AddTorrentRequest>,
) -> impl IntoResponse {
    if let Some(uri) = payload.uri {
        match session.add_magnet(&uri).await {
            Ok(id) => (StatusCode::CREATED, Json(id)).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: e.to_string(),
                }),
            )
                .into_response(),
        }
    } else if let Some(path) = payload.path {
        match session.add_torrent(&path).await {
            Ok(id) => (StatusCode::CREATED, Json(id)).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: e.to_string(),
                }),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "Missing 'path' or 'uri'".to_string(),
            }),
        )
            .into_response()
    }
}

async fn get_torrent(
    State(session): State<Arc<Session>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let info_hash = match InfoHash::from_hex(&id) {
        Ok(h) => h,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "Invalid InfoHash format".to_string(),
                }),
            )
                .into_response();
        }
    };

    match session.get_torrent_details(info_hash).await {
        Ok(Some(details)) => (StatusCode::OK, Json(details)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "Torrent not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn remove_torrent(
    State(session): State<Arc<Session>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let info_hash = match InfoHash::from_hex(&id) {
        Ok(h) => h,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "Invalid InfoHash format".to_string(),
                }),
            )
                .into_response();
        }
    };

    match session.remove_torrent(info_hash).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_stats(State(session): State<Arc<Session>>) -> impl IntoResponse {
    match session.get_stats().await {
        Ok(stats) => (StatusCode::OK, Json(stats)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
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
    info!("  API port: {}", args.api_port);
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

    // Build Axum router
    let app = Router::new()
        .route("/torrents", get(list_torrents).post(add_torrent))
        .route("/torrents/:id", get(get_torrent).delete(remove_torrent))
        .route("/stats", get(get_stats))
        .layer(CorsLayer::permissive())
        .with_state(session.clone());

    // Start API server
    let api_port = args.api_port;
    tokio::spawn(async move {
        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], api_port));
        info!("API server listening on {}", addr);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
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

async fn sigterm() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
    sigterm.recv().await;
}
