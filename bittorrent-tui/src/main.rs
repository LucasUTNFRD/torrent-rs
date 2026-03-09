mod app;
mod event;
mod torrent;
mod tui;
mod ui;

use anyhow::Result;
use clap::Parser;

use crate::app::App;
use crate::torrent::DaemonClient;

#[derive(Parser)]
#[command(name = "bittorrent-tui")]
struct Args {
    /// URL of the btd HTTP API
    #[arg(long, default_value = "http://localhost:6969")]
    daemon_url: String,

    /// Refresh interval in milliseconds
    #[arg(long, default_value_t = 1000)]
    refresh_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = DaemonClient::new(args.daemon_url);
    let mut app = App::new(client);

    let mut terminal = tui::init()?;

    // Run the event loop
    let res = event::run(&mut terminal, &mut app, args.refresh_ms).await;

    tui::restore(&mut terminal)?;

    res
}
