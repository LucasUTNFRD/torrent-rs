#[cfg(feature = "sim")]
mod mock;

#[cfg(feature = "sim")]
mod sim_tests {
    use bittorrent_common::metainfo::{FileMode, Info, TorrentInfo};
    use bittorrent_common::types::InfoHash;
    use bittorrent_core::{Session, SessionConfig};
    use sha1::{Digest, Sha1};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use turmoil;

    // Initialize tracing for all sim tests
    fn init_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
            )
            .with_test_writer()
            .try_init();
    }

    use crate::mock::peer::{UnchokeTiming, run_mock_peer};
    use crate::mock::storage::{MockStorage, generate_piece};

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
    fn test_swarm_star_topology() {
        init_tracing();
        let mut sim = turmoil::Builder::new().build();

        // Create a small test torrent: 4 pieces of 16 KiB each
        let torrent_info = make_test_torrent(16384, 4);
        let _info_hash = torrent_info.info_hash;
        let torrent_info = Arc::new(torrent_info);

        const NUM_PEERS: usize = 3;

        // -- Node 0: The central downloader --
        let dl_torrent = torrent_info.clone();
        sim.host("downloader", move || {
            let torrent = dl_torrent.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    peer_id: Some(bittorrent_common::types::PeerID::generate()),
                    ..Default::default()
                };

                let storage = Arc::new(MockStorage::new());
                let session = Session::builder(config).with_storage(storage).build();

                let id = session
                    .add_torrent_info((*torrent).clone())
                    .await
                    .expect("downloader: add torrent failed");

                // Connect to all peers
                for i in 1..=NUM_PEERS {
                    let peer_name = format!("peer-{}", i);
                    let peer_addr = turmoil::lookup(peer_name);
                    session
                        .connect_peer(id, std::net::SocketAddr::new(peer_addr, 6881))
                        .await
                        .expect("downloader: connect_peer failed");
                }

                // Wait for download to complete
                session
                    .wait_for_completion(id)
                    .await
                    .expect("downloader: wait for completion failed");

                Ok(())
            }
        });

        // -- Nodes 1..N: The peers (seeders) --
        for i in 1..=NUM_PEERS {
            let peer_name = format!("peer-{}", i);
            let p_torrent = torrent_info.clone();
            sim.host(peer_name.clone(), move || {
                let torrent = p_torrent.clone();
                async move {
                    let config = SessionConfig {
                        listen_addr: "0.0.0.0:6881".parse().unwrap(),
                        enable_dht: false,
                        peer_id: Some(bittorrent_common::types::PeerID::generate()),
                        ..Default::default()
                    };

                    let storage = Arc::new(MockStorage::all_have(
                        torrent.info_hash,
                        torrent.info.clone(),
                    ));

                    let session = Session::builder(config).with_storage(storage).build();

                    let _id = session
                        .seed_torrent_unchecked((*torrent).clone())
                        .await
                        .expect("peer: add torrent failed");

                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Ok(())
                }
            });
        }

        // -- Star Topology Isolation --
        // Partition peers from each other, but allow them to talk to downloader
        for i in 1..=NUM_PEERS {
            for j in (i + 1)..=NUM_PEERS {
                let p1 = format!("peer-{}", i);
                let p2 = format!("peer-{}", j);
                sim.partition(p1, p2);
            }
        }

        sim.run().expect("Simulation failed");
    }

    #[test]
    fn test_self_connect() {
        init_tracing();
        let mut sim = turmoil::Builder::new().build();

        let torrent_info = make_test_torrent(16384, 1);
        let torrent_info = Arc::new(torrent_info);

        // Both hosts use the SAME peer_id
        let shared_peer_id = bittorrent_common::types::PeerID::generate();

        // -- Node 0: Listener --
        let t1 = torrent_info.clone();
        sim.host("node-0", move || {
            let torrent = t1.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    peer_id: Some(shared_peer_id),
                    ..Default::default()
                };

                let session = Session::builder(config)
                    .with_storage(Arc::new(MockStorage::new()))
                    .build();
                let _id = session.add_torrent_info((*torrent).clone()).await.unwrap();

                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(())
            }
        });

        // -- Node 1: Connector --
        let t2 = torrent_info.clone();
        sim.host("node-1", move || {
            let torrent = t2.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    peer_id: Some(shared_peer_id), // Same ID!
                    ..Default::default()
                };

                let session = Session::builder(config)
                    .with_storage(Arc::new(MockStorage::new()))
                    .build();
                let id = session.add_torrent_info((*torrent).clone()).await.unwrap();

                let node0_addr = turmoil::lookup("node-0");
                session
                    .connect_peer(id, std::net::SocketAddr::new(node0_addr, 6881))
                    .await
                    .unwrap();

                // Wait until we observe the peer count dropping to 0 (self-connection rejected)
                let mut metrics_rx = session.subscribe_torrent(id).await.unwrap();
                loop {
                    if metrics_rx.borrow().peers_connected == 0 {
                        break;
                    }
                    metrics_rx.changed().await.unwrap();
                }

                // Check metrics: should have 0 connected peers
                let metrics = metrics_rx.borrow();
                assert_eq!(
                    metrics.peers_connected, 0,
                    "Self-connection should have been rejected"
                );

                Ok(())
            }
        });

        sim.run().expect("Simulation failed");
    }

    #[test]
    fn test_dsl_heterogeneity() {
        init_tracing();
        let mut sim = turmoil::Builder::new().build();

        // 20 pieces to see the effect of speed differences
        let torrent_info = make_test_torrent(16384, 20);
        let _info_hash = torrent_info.info_hash;
        let torrent_info = Arc::new(torrent_info);

        // -- Peer 1: Fast (Low latency) --
        let p1_torrent = torrent_info.clone();
        sim.host("fast-peer", move || {
            let torrent = p1_torrent.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    peer_id: Some(bittorrent_common::types::PeerID::generate()),
                    ..Default::default()
                };
                let storage = Arc::new(MockStorage::all_have(
                    torrent.info_hash,
                    torrent.info.clone(),
                ));
                let session = Session::builder(config).with_storage(storage).build();
                let _id = session
                    .seed_torrent_unchecked((*torrent).clone())
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(())
            }
        });

        // -- Peer 2: Slow (High latency) --
        let p2_torrent = torrent_info.clone();
        sim.host("slow-peer", move || {
            let torrent = p2_torrent.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    peer_id: Some(bittorrent_common::types::PeerID::generate()),
                    ..Default::default()
                };
                let storage = Arc::new(MockStorage::all_have(
                    torrent.info_hash,
                    torrent.info.clone(),
                ));
                let session = Session::builder(config).with_storage(storage).build();
                let _id = session
                    .seed_torrent_unchecked((*torrent).clone())
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(())
            }
        });

        // -- Downloader --
        let dl_torrent = torrent_info.clone();
        sim.host("downloader", move || {
            let torrent = dl_torrent.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    peer_id: Some(bittorrent_common::types::PeerID::generate()),
                    ..Default::default()
                };
                let storage = Arc::new(MockStorage::new());
                let session = Session::builder(config).with_storage(storage).build();

                let id = session.add_torrent_info((*torrent).clone()).await.unwrap();

                let fast_addr = turmoil::lookup("fast-peer");
                let slow_addr = turmoil::lookup("slow-peer");

                session
                    .connect_peer(id, std::net::SocketAddr::new(fast_addr, 6881))
                    .await
                    .unwrap();
                session
                    .connect_peer(id, std::net::SocketAddr::new(slow_addr, 6881))
                    .await
                    .unwrap();

                session.wait_for_completion(id).await.unwrap();
                Ok(())
            }
        });

        // Configure network links
        // Fast peer: 10ms latency
        // Slow peer: 500ms latency
        sim.set_link_latency("fast-peer", "downloader", Duration::from_millis(10));
        sim.set_link_latency("slow-peer", "downloader", Duration::from_millis(500));

        sim.run().expect("Simulation failed");
    }

    #[test]
    fn test_optimistic_unchoke() {
        init_tracing();

        const NUM_PEERS: usize = 20;
        // Each peer gets 90s worth of rotation opportunity
        const SIM_DURATION_SECS: u64 = (NUM_PEERS as u64) * 90;

        let torrent_info = make_test_torrent(16384, 2);
        let info_hash = torrent_info.info_hash;
        let torrent_info = Arc::new(torrent_info);

        // Shared timing data collected by mock peers
        let timing: UnchokeTiming = Arc::new(Mutex::new(HashMap::new()));

        let mut sim = turmoil::Builder::new()
            .simulation_duration(Duration::from_secs(SIM_DURATION_SECS + 60))
            // Increase tick duration from default (1ms) to speed up the simulation.
            // Our smallest meaningful timer is 1s (metrics), so 100ms resolution is fine.
            .tick_duration(Duration::from_millis(100))
            .build();

        // -- Seeder host --
        let seeder_torrent = torrent_info.clone();
        sim.host("seeder", move || {
            let torrent = seeder_torrent.clone();
            async move {
                let config = SessionConfig {
                    listen_addr: "0.0.0.0:6881".parse().unwrap(),
                    enable_dht: false,
                    // Only 1 slot => pure optimistic rotation
                    unchoke_slots_limit: 1,
                    peer_id: Some(bittorrent_common::types::PeerID::generate()),
                    ..Default::default()
                };

                let storage = Arc::new(MockStorage::all_have(
                    torrent.info_hash,
                    torrent.info.clone(),
                ));

                let session = Session::builder(config).with_storage(storage).build();

                let _id = session
                    .seed_torrent_unchecked((*torrent).clone())
                    .await
                    .expect("seeder: add torrent failed");

                tracing::info!("Seeder session is ready and listening");

                // Keep alive for the full simulation (bounded)
                tokio::time::sleep(Duration::from_secs(SIM_DURATION_SECS + 30)).await;
                Ok(())
            }
        });

        // -- Mock peer hosts --
        for i in 0..NUM_PEERS {
            let peer_name = format!("peer-{}", i);
            let peer_info_hash = info_hash;
            let peer_timing = timing.clone();

            sim.host(peer_name.clone(), move || {
                let name = peer_name.clone();
                let t = peer_timing.clone();
                async move {
                    // Wait for the seeder session to be fully up
                    tokio::time::sleep(Duration::from_secs(2)).await;

                    let seeder_addr = turmoil::lookup("seeder");
                    let addr = std::net::SocketAddr::new(seeder_addr, 6881);

                    if let Err(e) = run_mock_peer(name.clone(), addr, peer_info_hash, t).await {
                        tracing::error!("[{}] Mock peer failed: {}", name, e);
                    }
                    Ok(())
                }
            });
        }

        // -- Client: drives the simulation for the required duration --
        let client_timing = timing.clone();
        sim.client("test-driver", async move {
            tracing::info!("Test driver: waiting {}s for simulation", SIM_DURATION_SECS);

            // Wait for the full simulation duration
            tokio::time::sleep(Duration::from_secs(SIM_DURATION_SECS)).await;

            tracing::info!("Test driver: simulation complete, checking results");

            // -- Assertion: verify fair unchoke rotation --
            let results = client_timing.lock().unwrap();
            let num_results = results.len();

            eprintln!("=== Optimistic Unchoke Fairness ===");
            eprintln!("Peers reporting: {}/{}", num_results, NUM_PEERS);

            assert!(
                num_results > 0,
                "No peers stored timing data — mock peers likely failed to connect"
            );

            let total_ms: u128 = results.values().map(|d| d.as_millis()).sum();
            let average_ms = total_ms / num_results as u128;

            eprintln!("Average unchoke time: {}ms", average_ms);

            for (name, duration) in results.iter() {
                let ms = duration.as_millis();
                let diff = (ms as i128 - average_ms as i128).unsigned_abs();
                eprintln!("  {}: {}ms (diff from avg: {}ms)", name, ms, diff);

                assert!(
                    diff < 45000,
                    "Peer {} unchoke duration {}ms differs from average {}ms by {}ms (>45000ms)",
                    name,
                    ms,
                    average_ms,
                    diff
                );
            }

            Ok(())
        });

        // Run the simulation — returns when client completes
        sim.run().expect("Simulation failed");
    }
}
