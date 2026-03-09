//! BitTorrent CLI Controller
//!
//! A command-line tool to control the BitTorrent daemon.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bittorrent_common::types::InfoHash;
use bittorrent_core::types::{SessionStats, TorrentDetails, TorrentState, TorrentSummary};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::time::interval;
use tracing::info;
use tracing_subscriber::fmt::writer::MakeWriterExt;

#[derive(Parser)]
#[command(name = "bittorrent-cli")]
#[command(about = "BitTorrent CLI controller - control btd from the command line")]
#[command(version)]
struct Cli {
    /// URL of the btd HTTP API
    #[arg(long, default_value = "http://localhost:6969", global = true)]
    daemon_url: String,

    /// Set log level (error, warn, info, debug, trace)
    #[arg(long, value_enum, default_value_t = LogLevel::Info, global = true)]
    log_level: LogLevel,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a new torrent from file or magnet URI
    Add {
        /// Path to the torrent file or magnet URI
        torrent: String,
        /// Show live progress after adding
        #[arg(short, long)]
        follow: bool,
    },
    /// List all active torrents
    List,
    /// Show detailed info for a specific torrent
    Show {
        /// InfoHash of the torrent (hex string)
        id: String,
        /// Keep polling and show live progress
        #[arg(short, long)]
        follow: bool,
    },
    /// Remove a torrent from the daemon
    Remove {
        /// InfoHash of the torrent (hex string)
        id: String,
    },
    /// Show aggregate session statistics
    Stats,
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

#[derive(Serialize, Deserialize)]
struct AddTorrentRequest {
    path: Option<String>,
    uri: Option<String>,
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

        if self.state.needs_clear.load(Ordering::SeqCst) {
            let line = self.state.current_line.lock().unwrap();
            if !line.is_empty() {
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
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes < KB {
        format!("{} B", bytes)
    } else if bytes < MB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes < TB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    }
}

fn format_bytes_per_second(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

fn get_status_line(summary: &TorrentSummary) -> String {
    let progress = summary.progress * 100.0;
    let state_str = match summary.state {
        TorrentState::FetchingMetadata => "Fetching metadata".to_string(),
        TorrentState::Checking => {
            format!("Verifying local files ({:.1}%)", progress)
        }
        TorrentState::Downloading => {
            format!(
                "Progress: {:.1}%, dl from {} of {} peers ({}), ul to {} ({})",
                progress,
                summary.peers_connected,
                summary.peers_discovered,
                format_bytes_per_second(summary.download_rate),
                0, // seeding peers not yet tracked
                format_bytes_per_second(summary.upload_rate),
            )
        }
        TorrentState::Seeding => {
            format!(
                "Seeding, uploading to {} of {} peer(s), {}",
                0,
                summary.peers_discovered,
                format_bytes_per_second(summary.upload_rate),
            )
        }
        TorrentState::Paused => "Paused".to_string(),
        TorrentState::Error => "Error".to_string(),
    };

    format!("{:<80}", state_str)
}

struct ApiClient {
    client: reqwest::Client,
    base_url: String,
}

impl ApiClient {
    fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
        }
    }

    async fn add_torrent(&self, torrent: &str) -> Result<InfoHash, reqwest::Error> {
        let is_magnet = torrent.starts_with("magnet:");
        let payload = if is_magnet {
            AddTorrentRequest {
                uri: Some(torrent.to_string()),
                path: None,
            }
        } else {
            let full_path = std::fs::canonicalize(torrent)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| torrent.to_string());
            AddTorrentRequest {
                path: Some(full_path),
                uri: None,
            }
        };

        self.client
            .post(format!("{}/torrents", self.base_url))
            .json(&payload)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }

    async fn list_torrents(&self) -> Result<Vec<TorrentSummary>, reqwest::Error> {
        self.client
            .get(format!("{}/torrents", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }

    async fn get_torrent(&self, id: &str) -> Result<TorrentDetails, reqwest::Error> {
        self.client
            .get(format!("{}/torrents/{}", self.base_url, id))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }

    async fn remove_torrent(&self, id: &str) -> Result<(), reqwest::Error> {
        self.client
            .delete(format!("{}/torrents/{}", self.base_url, id))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn get_stats(&self) -> Result<SessionStats, reqwest::Error> {
        self.client
            .get(format!("{}/stats", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Set up status line handling
    let status_state = Arc::new(StatusState::new());
    let status_state_for_writer = status_state.clone();
    let log_level = tracing::Level::from(cli.log_level);

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

    let client = ApiClient::new(cli.daemon_url);

    match cli.command {
        Commands::Add { torrent, follow } => {
            println!("Adding torrent...");
            match client.add_torrent(&torrent).await {
                Ok(id) => {
                    let id_str = id.to_hex();
                    println!("Torrent added successfully: {}", id_str);
                    if follow {
                        follow_torrent(&client, &id_str, status_state).await?;
                    }
                }
                Err(e) => {
                    eprintln!("Error adding torrent: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::List => match client.list_torrents().await {
            Ok(torrents) => {
                if torrents.is_empty() {
                    println!("No active torrents.");
                } else {
                    println!(
                        "{:<40} {:<30} {:<10} {:<10}",
                        "ID", "Name", "Progress", "State"
                    );
                    for t in torrents {
                        println!(
                            "{:<40} {:<30} {:<10.1}% {:<10}",
                            t.id.to_hex(),
                            t.name,
                            t.progress * 100.0,
                            t.state.to_string()
                        );
                    }
                }
            }
            Err(e) => eprintln!("Error listing torrents: {}", e),
        },
        Commands::Show { id, follow } => {
            if follow {
                follow_torrent(&client, &id, status_state).await?;
            } else {
                match client.get_torrent(&id).await {
                    Ok(details) => {
                        println!("Torrent: {}", details.summary.name);
                        println!("ID:      {}", details.summary.id.to_hex());
                        println!("State:   {}", details.summary.state);
                        println!("Progress: {:.1}%", details.summary.progress * 100.0);
                        println!(
                            "Speed:    DL: {} / UL: {}",
                            format_bytes_per_second(details.summary.download_rate),
                            format_bytes_per_second(details.summary.upload_rate)
                        );

                        println!("\nFiles:");
                        for f in details.files {
                            println!(
                                "  - {} ({}, {:.1}%)",
                                f.path,
                                format_bytes(f.size),
                                f.progress * 100.0
                            );
                        }

                        println!("\nPeers ({}):", details.peers.len());
                        for p in details.peers {
                            println!(
                                "  - {} ({}): {} / {}",
                                p.ip,
                                p.client_id,
                                format_bytes_per_second(p.rate_down / 8), // bits -> bytes
                                format_bytes_per_second(p.rate_up / 8)
                            );
                        }

                        println!("\nTrackers:");
                        for t in details.trackers {
                            println!("  - {}: {}", t.url, t.error.as_deref().unwrap_or("OK"));
                        }
                    }
                    Err(e) => eprintln!("Error getting torrent details: {}", e),
                }
            }
        }
        Commands::Remove { id } => match client.remove_torrent(&id).await {
            Ok(_) => println!("Torrent {} removed.", id),
            Err(e) => eprintln!("Error removing torrent: {}", e),
        },
        Commands::Stats => match client.get_stats().await {
            Ok(stats) => {
                println!("Session Stats:");
                println!("  Downloading: {}", stats.torrents_downloading);
                println!("  Seeding:     {}", stats.torrents_seeding);
                println!(
                    "  Total DL:    {}",
                    format_bytes_per_second(stats.total_download_rate)
                );
                println!(
                    "  Total UL:    {}",
                    format_bytes_per_second(stats.total_upload_rate)
                );
                if let Some(nodes) = stats.dht_nodes {
                    println!("  DHT Nodes:   {}", nodes);
                }
            }
            Err(e) => eprintln!("Error getting stats: {}", e),
        },
    }

    Ok(())
}

async fn follow_torrent(
    client: &ApiClient,
    id: &str,
    status_state: Arc<StatusState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ticker = interval(Duration::from_millis(500));
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    tokio::spawn(async move {
        if let Err(_) = tokio::signal::ctrl_c().await {}
        let _ = shutdown_tx.send(()).await;
    });

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                match client.get_torrent(id).await {
                    Ok(details) => {
                        let line = get_status_line(&details.summary);
                        {
                            let mut current = status_state.current_line.lock().unwrap();
                            *current = line.clone();
                        }
                        status_state.needs_clear.store(true, Ordering::SeqCst);
                        print!("\r{}", line);
                        std::io::stdout().flush()?;
                    }
                    Err(e) => {
                        eprintln!("\nError polling torrent: {}", e);
                        break;
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                println!("\nDetaching from daemon...");
                break;
            }
        }
    }
    Ok(())
}
