use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
struct PeerMetrics {
    pub download_rate: AtomicU64, // bytes/sec
    pub bytes_downloaded: AtomicU64,
    pub last_activity: AtomicU64, // unix timestamp nanos
}

impl PeerMetrics {
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Clock may have gone backwards")
            .as_nanos() as u64;

        Self {
            download_rate: AtomicU64::new(0),
            bytes_downloaded: AtomicU64::new(0),
            last_activity: AtomicU64::new(now),
        }
    }

    pub fn update_download(&self, bytes: u64, rate: u64) {
        self.bytes_downloaded.fetch_add(bytes, Ordering::Relaxed);
        self.download_rate.store(rate, Ordering::Relaxed);
        self.touch_activity();
    }

    fn touch_activity(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        self.last_activity.store(now, Ordering::Relaxed);
    }

    pub fn get_download_rate(&self) -> u64 {
        self.download_rate.load(Ordering::Relaxed)
    }

    pub fn get_bytes_downloaded(&self) -> u64 {
        self.bytes_downloaded.load(Ordering::Relaxed)
    }

    /// converts an atomic timestamp back to a SystemTime
    pub fn get_last_activity(&self) -> SystemTime {
        let nanos = self.last_activity.load(Ordering::Relaxed);
        UNIX_EPOCH + Duration::from_nanos(nanos)
    }
}
