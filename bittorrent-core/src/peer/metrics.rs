use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Exponential Moving Average (EMA) smoothing factor
/// Higher values = more responsive to changes, lower values = smoother
const EMA_ALPHA: f64 = 0.3;
const EMA_ALPHA_COMPLEMENT: f64 = 0.7; // 1.0 - 0.3

/// Tracks peer upload/download metrics for choking decisions
#[derive(Debug)]
pub struct PeerMetrics {
    // Download tracking
    total_downloaded: AtomicU64,
    window_downloaded: AtomicU64,
    ema_download: AtomicU64, // f64 bits stored as u64
    last_update: AtomicU64,  // timestamp in milliseconds

    // Upload tracking
    total_uploaded: AtomicU64,
    window_uploaded: AtomicU64,
    ema_upload: AtomicU64, // f64 bits stored as u64

    // Choking algorithm specific tracking
    uploaded_in_last_round: AtomicU64,
    uploaded_since_unchoked: AtomicU64,
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
            total_uploaded: AtomicU64::new(0),
            window_uploaded: AtomicU64::new(0),
            ema_upload: AtomicU64::new(0.0f64.to_bits()),
            uploaded_in_last_round: AtomicU64::new(0),
            uploaded_since_unchoked: AtomicU64::new(0),
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

        // Update download EMA
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

        // Update upload EMA
        let uploaded = self.window_uploaded.swap(0, Ordering::Relaxed);
        let ul_rate = uploaded as f64 / elapsed;

        let ema_ul_bits = self.ema_upload.load(Ordering::Relaxed);
        let current_ema_ul = f64::from_bits(ema_ul_bits);

        let new_ema_ul = if current_ema_ul == 0.0 {
            ul_rate
        } else {
            EMA_ALPHA * ul_rate + EMA_ALPHA_COMPLEMENT * current_ema_ul
        };

        self.ema_upload
            .store(new_ema_ul.to_bits(), Ordering::Release);
    }

    // ==================== UPLOAD TRACKING ====================

    /// Record uploaded bytes
    pub fn record_upload(&self, bytes: u64) {
        self.total_uploaded.fetch_add(bytes, Ordering::Relaxed);
        self.window_uploaded.fetch_add(bytes, Ordering::Relaxed);
        self.uploaded_in_last_round
            .fetch_add(bytes, Ordering::Relaxed);
        self.uploaded_since_unchoked
            .fetch_add(bytes, Ordering::Relaxed);
    }

    /// Get upload rate as f64 (bytes per second)
    pub fn upload_rate_f64(&self) -> f64 {
        let bits = self.ema_upload.load(Ordering::Relaxed);
        f64::from_bits(bits)
    }

    /// Get upload rate as u64 (bytes per second, truncated)
    pub fn get_upload_rate(&self) -> u64 {
        self.upload_rate_f64() as u64
    }

    /// Get total bytes uploaded
    pub fn get_bytes_uploaded(&self) -> u64 {
        self.total_uploaded.load(Ordering::Relaxed)
    }

    /// Get upload rate in human-readable format
    pub fn get_upload_rate_human(&self) -> String {
        format_bytes_per_second(self.get_upload_rate())
    }

    /// Get bytes uploaded in human-readable format
    pub fn get_bytes_uploaded_human(&self) -> String {
        format_bytes(self.get_bytes_uploaded())
    }

    /// Get window uploaded bytes (since last update)
    pub fn window_uploaded(&self) -> u64 {
        self.window_uploaded.load(Ordering::Relaxed)
    }

    // ==================== CHOKING ALGORITHM TRACKING ====================

    /// Get bytes uploaded in the last unchoke round
    pub fn uploaded_in_last_round(&self) -> u64 {
        self.uploaded_in_last_round.load(Ordering::Relaxed)
    }

    /// Get bytes uploaded since this peer was last unchoked
    pub fn uploaded_since_unchoked(&self) -> u64 {
        self.uploaded_since_unchoked.load(Ordering::Relaxed)
    }

    /// Reset the "last round" counter. Called at each unchoke interval.
    pub fn reset_round_counters(&self) {
        self.uploaded_in_last_round.store(0, Ordering::Relaxed);
    }

    /// Reset the "since unchoked" counter. Called when peer is choked.
    pub fn reset_since_unchoked(&self) {
        self.uploaded_since_unchoked.store(0, Ordering::Relaxed);
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

    // ==================== UPLOAD TRACKING TESTS ====================

    #[test]
    fn test_record_upload() {
        let metrics = PeerMetrics::new();
        metrics.record_upload(1024);
        assert_eq!(metrics.get_bytes_uploaded(), 1024);
        assert_eq!(metrics.window_uploaded(), 1024);
        assert_eq!(metrics.uploaded_in_last_round(), 1024);
        assert_eq!(metrics.uploaded_since_unchoked(), 1024);
    }

    #[test]
    fn test_upload_rate_calculation() {
        let metrics = PeerMetrics::new();

        // Record some uploads
        metrics.record_upload(2048);
        thread::sleep(Duration::from_millis(100));

        // Update rates
        metrics.update_rates();

        assert_eq!(metrics.get_bytes_uploaded(), 2048);
        // Rate should be calculated (though may vary due to timing)
        assert!(metrics.upload_rate_f64() >= 0.0);
    }

    #[test]
    fn test_reset_round_counters() {
        let metrics = PeerMetrics::new();

        metrics.record_upload(1024);
        assert_eq!(metrics.uploaded_in_last_round(), 1024);
        assert_eq!(metrics.uploaded_since_unchoked(), 1024);

        metrics.reset_round_counters();
        assert_eq!(metrics.uploaded_in_last_round(), 0);
        assert_eq!(metrics.uploaded_since_unchoked(), 1024); // Should remain

        // New uploads after reset
        metrics.record_upload(512);
        assert_eq!(metrics.uploaded_in_last_round(), 512);
        assert_eq!(metrics.uploaded_since_unchoked(), 1536);
    }

    #[test]
    fn test_reset_since_unchoked() {
        let metrics = PeerMetrics::new();

        metrics.record_upload(1024);
        assert_eq!(metrics.uploaded_since_unchoked(), 1024);

        metrics.reset_since_unchoked();
        assert_eq!(metrics.uploaded_since_unchoked(), 0);
        assert_eq!(metrics.uploaded_in_last_round(), 1024); // Should remain
    }

    #[test]
    fn test_upload_tracking_persists_across_rounds() {
        let metrics = PeerMetrics::new();

        // Simulate uploads across multiple rounds
        metrics.record_upload(1000);
        assert_eq!(metrics.uploaded_in_last_round(), 1000);
        assert_eq!(metrics.uploaded_since_unchoked(), 1000);

        metrics.reset_round_counters();
        metrics.record_upload(500);
        assert_eq!(metrics.uploaded_in_last_round(), 500);
        assert_eq!(metrics.uploaded_since_unchoked(), 1500);

        metrics.reset_round_counters();
        metrics.record_upload(200);
        assert_eq!(metrics.uploaded_in_last_round(), 200);
        assert_eq!(metrics.uploaded_since_unchoked(), 1700);

        // Simulate peer getting choked and unchoked again
        metrics.reset_since_unchoked();
        assert_eq!(metrics.uploaded_since_unchoked(), 0);
        assert_eq!(metrics.uploaded_in_last_round(), 200);
    }

    #[test]
    fn test_upload_download_independent() {
        let metrics = PeerMetrics::new();

        metrics.record_download(1000);
        metrics.record_upload(500);

        assert_eq!(metrics.get_bytes_downloaded(), 1000);
        assert_eq!(metrics.get_bytes_uploaded(), 500);
        assert_eq!(metrics.window_downloaded(), 1000);
        assert_eq!(metrics.window_uploaded(), 500);
    }

    #[test]
    fn test_get_upload_rate_human() {
        let metrics = PeerMetrics::new();
        // Just verify it doesn't panic and returns a string
        let rate_str = metrics.get_upload_rate_human();
        assert!(rate_str.contains("/s"));
    }
}
