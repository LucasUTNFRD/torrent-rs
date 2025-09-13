use magnet_uri::Magnet;
use std::str::FromStr;

fn main() {
    // A sample magnet URI for the Sintel movie
    let magnet_uri = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Sintel&tr=udp%3A%2F%2Fexplodie.org%3A6969&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969";

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
