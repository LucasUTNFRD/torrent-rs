use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "torrent-rs")]
#[command(about = "BitTorrent client CLI", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[arg(
        global = true,
        long,
        default_value = "0.0.0.0:9000",
        help = "Prometheus metrics listen address"
    )]
    pub metrics_addr: String,
}

#[derive(Subcommand)]
pub enum Commands {
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
