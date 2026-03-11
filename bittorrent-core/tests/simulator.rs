#[cfg(feature = "sim")]
#[path = "../tests/mock/mod.rs"]
mod mock;

#[cfg(feature = "sim")]
mod sim_tests {
    use bittorrent_common::metainfo::{FileMode, Info, TorrentInfo};
    use bittorrent_common::types::InfoHash;
    use bittorrent_core::{Session, SessionConfig};
    use sha1::{Digest, Sha1};
    use std::sync::Arc;
    use turmoil;

    use crate::mock::storage::{generate_piece, MockStorage};

    /// Build a synthetic single-file TorrentInfo with deterministic piece data.
    fn make_test_torrent(piece_length: u32, num_pieces: usize) -> TorrentInfo {
        let total_length = piece_length as i64 * num_pieces as i64;

        // Compute SHA-1 hashes from the deterministic generator
        let pieces: Vec<[u8; 20]> = (0..num_pieces)
            .map(|i| {
                let data = generate_piece(i as u32, piece_length);
                let mut hasher = Sha1::new();
                hasher.update(&data);
                hasher.finalize().into()
            })
            .collect();

        let info = Arc::new(Info {
            piece_length: piece_length as i64,
            pieces,
            private: None,
            mode: FileMode::SingleFile {
                name: "test_file.bin".to_string(),
                length: total_length,
                md5sum: None,
            },
        });

        // Compute info_hash from bencoded info dict
        let bencoded = bencode::Bencode::encode(&*info);
        let info_hash_bytes: [u8; 20] = Sha1::digest(&bencoded).into();

        TorrentInfo {
            info,
            announce: String::new(), // No tracker in simulation
            announce_list: None,
            creation_date: None,
            comment: None,
            created_by: None,
            encoding: None,
            info_hash: InfoHash::new(info_hash_bytes),
        }
    }

    #[test]
    fn test_seeder_to_downloader_transfer() {
        let mut sim = turmoil::Builder::new().build();

        // Create a small test torrent: 2 pieces of 16 KiB each
        let torrent_info = make_test_torrent(16384, 2);
        let _info_hash = torrent_info.info_hash;
        let torrent_info = Arc::new(torrent_info);

        // -- Seeder host --
        let seeder_torrent = torrent_info.clone();
        sim.host("seeder", move || {
            let torrent = seeder_torrent.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    ..Default::default()
                };

                let storage = Arc::new(MockStorage::all_have(
                    torrent.info_hash,
                    torrent.info.clone(),
                ));

                let session = Session::builder(config)
                    .with_storage(storage)
                    .build();

                // Add torrent as a completed seed
                let _id = session
                    .seed_torrent((*torrent).clone(), "/mock/content".into())
                    .await
                    .expect("seeder: add torrent failed");

                // Keep the host alive
                std::future::pending::<()>().await;
                Ok(())
            }
        });

        // -- Downloader host --
        let dl_torrent = torrent_info.clone();
        sim.host("downloader", move || {
            let torrent = dl_torrent.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    ..Default::default()
                };

                let storage = Arc::new(MockStorage::new());

                let session = Session::builder(config)
                    .with_storage(storage)
                    .build();

                // Add torrent as a download
                let id = session
                    .add_torrent_info((*torrent).clone())
                    .await
                    .expect("downloader: add torrent failed");

                // Resolve seeder address and connect
                let seeder_addr = turmoil::lookup("seeder");
                session
                    .connect_peer(id, std::net::SocketAddr::new(seeder_addr, 6881))
                    .await
                    .expect("downloader: connect_peer failed");

                // Wait for download to complete
                session
                    .wait_for_completion(id)
                    .await
                    .expect("downloader: wait for completion failed");

                Ok(())
            }
        });

        // Run the simulation until completion or timeout
        sim.run().expect("Simulation failed");
    }
}
