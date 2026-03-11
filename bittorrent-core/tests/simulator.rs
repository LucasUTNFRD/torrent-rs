#[cfg(feature = "sim")]
mod sim_tests {
    use bittorrent_common::metainfo::TorrentInfo;
    use bittorrent_core::{Session, SessionConfig, storage::StorageBackend};
    use std::sync::Arc;
    use turmoil;

    // 1. Create a simple MockStorage for the simulation
    // This stays in memory so tests are lightning fast.
    struct MemoryStorage;
    #[async_trait::async_trait]
    impl StorageBackend for MemoryStorage {
        // ... Implement just enough to return "Success" for reads/writes ...
    }

    #[test]
    fn test_seeder_to_downloader_transfer() {
        let mut sim = turmoil::Builder::new().build();

        // 2. Setup the Seeder Host (10.0.0.1)
        sim.host("seeder", || async {
            let config = SessionConfig {
                listen_addr: "10.0.0.1:6881".parse().unwrap(),
                ..Default::default()
            };

            let session = Session::builder(config)
                .with_storage(Arc::new(MemoryStorage))
                .with_seed(42) // Deterministic PeerID/Randomness
                .build();

            // Logic to add a "Completed" torrent here...

            // Keep the host alive
            std::future::pending::<()>().await;
            Ok(())
        });

        // 3. Setup the Downloader Host (10.0.0.2)
        sim.host("downloader", || async {
            let config = SessionConfig {
                listen_addr: "10.0.0.2:6881".parse().unwrap(),
                ..Default::default()
            };

            let session = Session::builder(config)
                .with_storage(Arc::new(MemoryStorage))
                .with_seed(43)
                .build();

            // Connect to the seeder using its virtual hostname
            // turmoil::net::TcpStream::connect("seeder:6881").await?;

            // Wait for download to finish...
            Ok(())
        });

        // 4. Run the simulation until completion or timeout
        sim.run().expect("Simulation failed");
    }
}
