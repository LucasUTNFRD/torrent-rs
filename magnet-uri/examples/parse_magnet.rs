use magnet_uri::Magnet;
use std::str::FromStr;

fn main() {
    // A sample magnet URI for Neuromancer read aloud by William Gibson
    let magnet_uri = "magnet:?xt=urn:btih:F939FC4D57AA6742431E3093BD181CE69C603B2E&dn=Neuromancer+read+aloud+by+William+Gibson&tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&tr=udp%3A%2F%2F47.ip-51-68-199.eu%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Fopentracker.i2p.rocks%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.dler.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce";

    match Magnet::from_str(magnet_uri) {
        Ok(magnet) => {
            println!("Successfully parsed magnet URI!");

            // Display the name of the torrent
            if let Some(name) = &magnet.display_name {
                println!("Torrent name: {}", name);
            } else {
                println!("No name provided in the magnet URI");
            }

            // Display the infohash
            if let Some(infohash) = magnet.info_hash() {
                println!("InfoHash: {}", infohash);
            }

            // List all trackers
            println!("\nTrackers ({}):", magnet.trackers.len());
            for (i, tracker) in magnet.trackers.iter().enumerate() {
                println!("  {}: {}", i + 1, tracker);
            }

            // List all peers
            println!("\nPeers ({}):", magnet.peers.len());
            for (i, peer) in magnet.peers.iter().enumerate() {
                println!("  {}: {}", i + 1, peer);
            }
        }
        Err(e) => {
            eprintln!("Failed to parse magnet URI: {}", e);
        }
    }

    // Try with an invalid URI
    let invalid_uri = "magnet:?invalid=param";
    match Magnet::from_str(invalid_uri) {
        Ok(_) => println!("\nInvalid URI parsed successfully (shouldn't happen)"),
        Err(e) => println!("\nAs expected, invalid URI failed to parse: {}", e),
    }
}
