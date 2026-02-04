use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Parser)]
#[command(name = "bittorrent-remote")]
#[command(about = "Remote control for the BitTorrent daemon")]
struct Cli {
    #[arg(long, default_value_t = 6969)]
    port: u16,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a torrent from a .torrent file
    Add {
        /// Path to the .torrent file
        path: PathBuf,
    },
    /// Add a torrent from a magnet URI
    AddMagnet {
        /// Magnet URI
        uri: String,
    },
    /// List all torrents
    List,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let command = match cli.command {
        Commands::Add { path } => IpcCommand::AddTorrent {
            path: path.to_string_lossy().to_string(),
        },
        Commands::AddMagnet { uri } => IpcCommand::AddMagnet { uri },
        Commands::List => IpcCommand::ListTorrents,
    };

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", cli.port)).await?;

    let req = serde_json::to_vec(&command)?;
    stream.write_all(&req).await?;
    stream.shutdown().await?;

    let mut resp_json = String::new();
    stream.read_to_string(&mut resp_json).await?;

    if resp_json.is_empty() {
        println!("No response from daemon");
    } else {
        let resp: IpcResponse = serde_json::from_str(&resp_json)?;
        match resp {
            IpcResponse::Success { message } => println!("Success: {}", message),
            IpcResponse::Error { message } => eprintln!("Error: {}", message),
            IpcResponse::TorrentList { torrents } => {
                println!("Torrents:");
                for t in torrents {
                    println!("  {}", t);
                }
            }
        }
    }

    Ok(())
}
