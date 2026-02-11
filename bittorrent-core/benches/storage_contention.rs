//! Benchmark: Storage actor serialization bottleneck
//!
//! Simulates N concurrent torrent sessions against a single shared Storage
//! actor. Two scenarios:
//!
//! ## Scenario 1: Downloading (verify + write)
//!
//! Each torrent session verifies then writes each piece sequentially.
//! This mirrors `torrent.rs::on_complete_piece()`:
//!   1. ``verify_piece`` -- SHA1 hash (CPU-bound), blocks on oneshot response
//!   2. ``write_piece``  -- disk I/O, fire-and-forget
//!
//! ## Scenario 2: Seeding (random block reads)
//!
//! Each torrent session serves random 16 KiB block reads to peers.
//! This mirrors a seeding torrent responding to `Request` messages:
//!   1. ``read_block`` -- disk I/O, blocks on oneshot response
//!
//! ## Page cache considerations
//!
//! All reads hit the OS page cache (data was recently written during setup).
//! This is intentional:
//!   - We're measuring **actor contention**, not raw disk throughput.
//!   - Page cache gives stable, reproducible measurements.
//!   - Even with warm cache, the serialization bottleneck is visible because
//!     the single storage thread still processes requests one at a time.
//!   - A real seeding client often serves data that IS cached (popular torrents,
//!     recently completed downloads, OS read-ahead).
//!
//! Write benchmarks are similarly honest: `write_all_at()` returns after
//! memcpy to kernel buffers (no fsync). This matches real client behavior --
//! libtorrent doesn't fsync per piece either.

use std::{
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use bittorrent_common::{
    metainfo::{FileMode, Info},
    types::InfoHash,
};
use bittorrent_core::Storage;
use criterion::{BenchmarkId, Criterion, criterion_group, measurement::WallTime};
use peer_protocol::protocol::BlockInfo;
use sha1::{Digest, Sha1};
use tokio::sync::{Mutex, oneshot};

const PIECE_LENGTH: usize = 256 * 1024; // 256 KiB -- typical piece size
const PIECES_PER_TORRENT: usize = 20; // 20 pieces = 5 MiB per torrent
const BLOCK_LENGTH: u32 = 16 * 1024; // 16 KiB -- standard BitTorrent block
const READS_PER_SESSION: usize = 40; // 40 random block reads per seeding session

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a fake single-file torrent with real SHA1 hashes so verification passes.
fn make_torrent_info(
    torrent_index: usize,
    piece_length: usize,
    num_pieces: usize,
) -> (InfoHash, Arc<Info>, Vec<Arc<[u8]>>) {
    let mut pieces_hashes = Vec::with_capacity(num_pieces);
    let mut pieces_data = Vec::with_capacity(num_pieces);

    for i in 0..num_pieces {
        let data: Vec<u8> = vec![(i & 0xFF) as u8; piece_length];
        let mut hasher = Sha1::new();
        hasher.update(&data);
        let hash: [u8; 20] = hasher.finalize().into();

        pieces_hashes.push(hash);
        pieces_data.push(Arc::from(data.into_boxed_slice()));
    }

    let mut hash_bytes = [0u8; 20];
    hash_bytes[0] = (torrent_index & 0xFF) as u8;
    hash_bytes[1] = ((torrent_index >> 8) & 0xFF) as u8;
    let info_hash = InfoHash::new(hash_bytes);

    let info = Info {
        piece_length: piece_length as i64,
        pieces: pieces_hashes,
        private: None,
        mode: FileMode::SingleFile {
            name: format!("bench_torrent_{torrent_index}.dat"),
            length: (piece_length * num_pieces) as i64,
            md5sum: None,
        },
    };

    (info_hash, Arc::new(info), pieces_data)
}

/// RAII temp directory that cleans up on drop.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct BenchEnv {
    storage: Arc<Storage>,
    setups: Vec<(InfoHash, Vec<Arc<[u8]>>)>,
    _tmp_dir: TempDir,
}

/// Prepare a Storage actor with N registered torrents.
fn setup_env(num_torrents: usize) -> BenchEnv {
    let tmp_dir = env::temp_dir().join(format!(
        "bt_bench_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let storage = Arc::new(Storage::with_download_dir(tmp_dir.clone()));

    let mut setups = Vec::with_capacity(num_torrents);
    for t in 0..num_torrents {
        let (info_hash, info, pieces_data) = make_torrent_info(t, PIECE_LENGTH, PIECES_PER_TORRENT);

        storage.add_torrent(info_hash, info);
        setups.push((info_hash, pieces_data));
    }

    // Give storage thread time to process AddTorrent messages.
    std::thread::sleep(Duration::from_millis(50));

    BenchEnv {
        storage,
        setups,
        _tmp_dir: TempDir(tmp_dir),
    }
}

/// Write all pieces for all torrents so they're on disk (and in page cache)
/// for the seeding benchmark.
async fn seed_all_pieces(env: &BenchEnv) {
    for (info_hash, pieces_data) in &env.setups {
        for (idx, piece_data) in pieces_data.iter().enumerate() {
            env.storage
                .write_piece(*info_hash, idx as u32, piece_data.clone());
        }
    }
    // Let all writes drain through the storage thread
    tokio::time::sleep(Duration::from_millis(100)).await;
}

// ---------------------------------------------------------------------------
// Scenario 1: Downloading (verify + write)
// ---------------------------------------------------------------------------

/// One torrent session downloading: verify then write each piece.
async fn simulate_download_session(
    storage: Arc<Storage>,
    info_hash: InfoHash,
    pieces_data: Vec<Arc<[u8]>>,
    latencies: Option<Arc<Mutex<Vec<Duration>>>>,
) {
    for (idx, piece_data) in pieces_data.into_iter().enumerate() {
        let (tx, rx) = oneshot::channel();
        let t0 = Instant::now();
        storage.verify_piece(info_hash, idx as u32, piece_data.clone(), tx);
        let valid = rx.await.expect("storage actor alive");
        let elapsed = t0.elapsed();

        assert!(valid, "piece {idx} should pass verification");

        if let Some(ref lats) = latencies {
            lats.lock().await.push(elapsed);
        }

        storage.write_piece(info_hash, idx as u32, piece_data);
    }
}

// ---------------------------------------------------------------------------
// Scenario 2: Seeding (random block reads)
// ---------------------------------------------------------------------------

/// One torrent session seeding: serve random block reads.
///
/// Each read is request-response via oneshot, so the torrent task blocks
/// until the storage thread processes it. With a single storage thread,
/// reads from different torrents queue behind each other.
async fn simulate_seed_session(
    storage: Arc<Storage>,
    info_hash: InfoHash,
    num_reads: usize,
    num_pieces: usize,
    piece_length: usize,
    latencies: Option<Arc<Mutex<Vec<Duration>>>>,
) {
    // Deterministic "random" block requests: cycle through pieces and offsets.
    // Using a simple LCG-style pattern rather than rand to avoid the dependency
    // in the benchmark and keep it reproducible.
    let blocks_per_piece = piece_length as u32 / BLOCK_LENGTH;

    for i in 0..num_reads {
        let piece_index = (i * 7 + 3) % num_pieces; // pseudo-random piece
        let block_in_piece = (i * 13 + 5) % blocks_per_piece as usize;
        let begin = block_in_piece as u32 * BLOCK_LENGTH;

        let block_info = BlockInfo {
            index: piece_index as u32,
            begin,
            length: BLOCK_LENGTH,
        };

        let (tx, rx) = oneshot::channel();
        let t0 = Instant::now();
        storage.read_block(info_hash, block_info, tx);
        let result = rx.await.expect("storage actor alive");
        let elapsed = t0.elapsed();

        assert!(result.is_ok(), "read should succeed");

        if let Some(ref lats) = latencies {
            lats.lock().await.push(elapsed);
        }
    }
}

// ---------------------------------------------------------------------------
// Criterion benchmarks
// ---------------------------------------------------------------------------

fn bench_download(c: &mut Criterion<WallTime>) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("download");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));

    for num_torrents in [1, 2, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::new("verify+write", num_torrents),
            &num_torrents,
            |b, &n| {
                b.to_async(&rt).iter_custom(|iters| async move {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let env = setup_env(n);
                        let start = Instant::now();

                        let mut handles = Vec::with_capacity(n);
                        for (info_hash, pieces_data) in env.setups.clone() {
                            let storage = env.storage.clone();
                            handles.push(tokio::spawn(simulate_download_session(
                                storage,
                                info_hash,
                                pieces_data,
                                None,
                            )));
                        }
                        for h in handles {
                            h.await.unwrap();
                        }

                        total += start.elapsed();
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

fn bench_seeding(c: &mut Criterion<WallTime>) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("seeding");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));

    for num_torrents in [1, 2, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::new("random_reads", num_torrents),
            &num_torrents,
            |b, &n| {
                b.to_async(&rt).iter_custom(|iters| async move {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        // Setup: write all data to disk first
                        let env = setup_env(n);
                        seed_all_pieces(&env).await;

                        // Measured: N concurrent seeding sessions doing random reads
                        let start = Instant::now();

                        let mut handles = Vec::with_capacity(n);
                        for (info_hash, _) in &env.setups {
                            let storage = env.storage.clone();
                            let info_hash = *info_hash;
                            handles.push(tokio::spawn(simulate_seed_session(
                                storage,
                                info_hash,
                                READS_PER_SESSION,
                                PIECES_PER_TORRENT,
                                PIECE_LENGTH,
                                None,
                            )));
                        }
                        for h in handles {
                            h.await.unwrap();
                        }

                        total += start.elapsed();
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Latency reports (printed after criterion)
// ---------------------------------------------------------------------------

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p / 100.0).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn print_latency_table(
    label: &str,
    torrent_counts: &[usize],
    collect_fn: impl Fn(usize) -> Vec<Duration>,
) {
    println!();
    println!("=== {label} ===");
    println!(
        "{:<10} {:>10} {:>10} {:>10} {:>10}",
        "Torrents", "p50", "p90", "p99", "max"
    );
    println!("{}", "-".repeat(54));

    for &n in torrent_counts {
        let mut lats = collect_fn(n);
        lats.sort();

        println!(
            "{:<10} {:>8.1?} {:>8.1?} {:>8.1?} {:>8.1?}",
            n,
            percentile(&lats, 50.0),
            percentile(&lats, 90.0),
            percentile(&lats, 99.0),
            lats.last().unwrap_or(&Duration::ZERO),
        );
    }
}

fn latency_report() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let counts = [1, 4, 16];

    // Verify latency (downloading)
    print_latency_table(
        "Verify latency -- downloading (head-of-line blocking)",
        &counts,
        |n| {
            let env = setup_env(n);
            let latencies: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));

            rt.block_on(async {
                let mut handles = Vec::new();
                for (info_hash, pieces_data) in &env.setups {
                    let storage = env.storage.clone();
                    let lats = latencies.clone();
                    handles.push(tokio::spawn(simulate_download_session(
                        storage,
                        *info_hash,
                        pieces_data.clone(),
                        Some(lats),
                    )));
                }
                for h in handles {
                    h.await.unwrap();
                }
            });

            Arc::try_unwrap(latencies).unwrap().into_inner()
        },
    );

    // Read latency (seeding)
    print_latency_table(
        "Read latency -- seeding (head-of-line blocking, warm page cache)",
        &counts,
        |n| {
            let env = setup_env(n);
            let latencies: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));

            rt.block_on(async {
                seed_all_pieces(&env).await;

                let mut handles = Vec::new();
                for (info_hash, _) in &env.setups {
                    let storage = env.storage.clone();
                    let lats = latencies.clone();
                    handles.push(tokio::spawn(simulate_seed_session(
                        storage,
                        *info_hash,
                        READS_PER_SESSION,
                        PIECES_PER_TORRENT,
                        PIECE_LENGTH,
                        Some(lats),
                    )));
                }
                for h in handles {
                    h.await.unwrap();
                }
            });

            Arc::try_unwrap(latencies).unwrap().into_inner()
        },
    );

    println!();
    println!("With perfect concurrency, latency should stay constant as N grows.");
    println!("Growing p50/p99 = head-of-line blocking on the single storage thread.");
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

criterion_group!(benches, bench_download, bench_seeding);

fn main() {
    benches();
    latency_report();
    Criterion::default().configure_from_args().final_summary();
}
