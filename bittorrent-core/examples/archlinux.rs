use bittorrent_core::{Session, SessionConfig};

const MAGNET_URL: &str = "magnet:?xt=urn:btih:a4373c326657898d0c588c3ff892a0fac97ffa20&dn=archlinux-2026.03.01-x86_64.iso";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session_cfg = SessionConfig::default();
    let session = Session::new(session_cfg);

    let torrent_ih = session.add_magnet(MAGNET_URL).await?;
    session.wait_for_completion(torrent_ih).await?;

    println!("Download finished");
    Ok(())
}
