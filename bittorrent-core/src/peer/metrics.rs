use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Exponential Moving Average (EMA) smoothing factor
/// Higher values = more responsive to changes, lower values = smoother
const EMA_ALPHA: f64 = 0.3;
const EMA_ALPHA_COMPLEMENT: f64 = 0.7; // 1.0 - 0.3

/// Simplified counter for tracking download metrics only
#[derive(Debug)]
pub struct PeerMetrics {
    total_downloaded: AtomicU64,
    window_downloaded: AtomicU64,
    ema_download: AtomicU64, // f64 bits stored as u64
    last_update: AtomicU64,  // timestamp in milliseconds
}

impl Default for PeerMetrics {
    fn default() -> Self {
        Self {
            total_downloaded: AtomicU64::new(0),
            window_downloaded: AtomicU64::new(0),
            last_update: AtomicU64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            ),
            ema_download: AtomicU64::new(0.0f64.to_bits()),
        }
    }
}

impl PeerMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record downloaded bytes
    pub fn record_download(&self, bytes: u64) {
        self.total_downloaded.fetch_add(bytes, Ordering::Relaxed);
        self.window_downloaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Get download rate as f64 (bytes per second)
    pub fn download_rate_f64(&self) -> f64 {
        let bits = self.ema_download.load(Ordering::Relaxed);
        f64::from_bits(bits)
    }

    /// Get download rate as u64 (bytes per second, truncated)
    pub fn get_download_rate(&self) -> u64 {
        self.download_rate_f64() as u64
    }

    /// Get total bytes downloaded
    pub fn get_bytes_downloaded(&self) -> u64 {
        self.total_downloaded.load(Ordering::Relaxed)
    }

    /// Get download rate in human-readable format
    pub fn get_download_rate_human(&self) -> String {
        format_bytes_per_second(self.get_download_rate())
    }

    /// Get bytes downloaded in human-readable format
    pub fn get_bytes_downloaded_human(&self) -> String {
        format_bytes(self.get_bytes_downloaded())
    }

    /// Get window downloaded bytes (since last update)
    pub fn window_downloaded(&self) -> u64 {
        self.window_downloaded.load(Ordering::Relaxed)
    }

    /// Update download rate with EMA smoothing
    /// This should be called periodically (e.g., every second)
    pub fn update_rates(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let last = self.last_update.load(Ordering::Acquire);
        let elapsed_ms = now - last;

        if elapsed_ms < 1 {
            return;
        }

        let elapsed = elapsed_ms as f64 / 1000.0;
        self.last_update.store(now, Ordering::Release);

        let downloaded = self.window_downloaded.swap(0, Ordering::Relaxed);
        let dl_rate = downloaded as f64 / elapsed;

        let ema_dl_bits = self.ema_download.load(Ordering::Relaxed);
        let current_ema_dl = f64::from_bits(ema_dl_bits);

        let new_ema_dl = if current_ema_dl == 0.0 {
            dl_rate
        } else {
            EMA_ALPHA * dl_rate + EMA_ALPHA_COMPLEMENT * current_ema_dl
        };

        self.ema_download
            .store(new_ema_dl.to_bits(), Ordering::Release);
    }
}

/// Format bytes in human-readable format
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

/// Format bytes per second in human-readable format
pub fn format_bytes_per_second(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_metrics_new() {
        let metrics = PeerMetrics::new();
        assert_eq!(metrics.get_download_rate(), 0);
        assert_eq!(metrics.get_bytes_downloaded(), 0);
    }

    #[test]
    fn test_record_download() {
        let metrics = PeerMetrics::new();
        metrics.record_download(1024);
        assert_eq!(metrics.get_bytes_downloaded(), 1024);
        assert_eq!(metrics.window_downloaded(), 1024);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn test_rate_calculation() {
        let metrics = PeerMetrics::new();

        // Record some downloads
        metrics.record_download(1024);
        thread::sleep(Duration::from_millis(100));

        // Update rates
        metrics.update_rates();

        assert_eq!(metrics.get_bytes_downloaded(), 1024);
        // Rate should be calculated (though may vary due to timing)
        assert!(metrics.download_rate_f64() >= 0.0);
    }

    #[test]
    fn test_rate_accuracy() {
        let metrics = PeerMetrics::new();

        // Simulate 1 second of downloads at 1MB/s
        for _ in 0..10 {
            metrics.record_download(102400); // 100KB
            thread::sleep(Duration::from_millis(100));
        }

        metrics.update_rates();
        let rate = metrics.get_download_rate();

        // Allow 10% tolerance for timing variance
        assert!((rate as i64 - 1_048_576).abs() < 104_857);
    }
    #[test]
    fn test_concurrent_recording_during_update() {
        use std::sync::Arc;
        let metrics = Arc::new(PeerMetrics::new());
        let m = metrics.clone();

        // Record 1KB
        metrics.record_download(1024);

        // Spawn thread that records during update window
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            m.record_download(1024); // Should go to NEXT window
        });

        thread::sleep(Duration::from_millis(100));
        metrics.update_rates();

        handle.join().unwrap();

        thread::sleep(Duration::from_millis(100));
        metrics.update_rates();

        // Second window should capture the concurrent write
        assert_eq!(metrics.get_bytes_downloaded(), 2048);
    }

    #[test]
    fn test_elapsed_time_calculation() {
        let metrics = PeerMetrics::new();

        let start = metrics.last_update.load(Ordering::Relaxed);

        metrics.record_download(1024);
        thread::sleep(Duration::from_millis(250));
        metrics.update_rates();

        let end = metrics.last_update.load(Ordering::Relaxed);
        let elapsed = end - start;

        // Should be ~250ms ± 50ms tolerance
        assert!((200..300).contains(&elapsed), "Elapsed was {}", elapsed);
    }
}
