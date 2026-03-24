use crate::ema::EmaRate;

/// Tracks per-peer upload/download rates and choking algorithm counters.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct PeerMetrics {
    download: EmaRate,
    total_downloaded: u64,

    upload: EmaRate,
    total_uploaded: u64,

    // Choking algorithm counters — not rate-related
    uploaded_in_last_round: u64,
    uploaded_since_unchoked: u64,
}

impl PeerMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset timing windows. Call when a peer slot is reused.
    #[allow(dead_code)]
    pub fn reset_timing(&mut self) {
        self.download.reset();
        self.upload.reset();
    }

    // ── Download ─────────────────────────────────────────────────────────────

    pub fn record_download(&mut self, bytes: u64) {
        self.total_downloaded += bytes;
        self.download.record(bytes);
    }

    /// Flush window and recompute download EMA. Call on a periodic tick.
    pub fn update_rates(&mut self) {
        self.download.update();
        self.upload.update();
    }

    pub fn download_rate_f64(&self) -> f64 {
        self.download.rate()
    }

    #[allow(dead_code)]
    pub fn get_download_rate(&self) -> u64 {
        self.download.rate() as u64
    }

    #[allow(dead_code)]
    pub fn get_bytes_downloaded(&self) -> u64 {
        self.total_downloaded
    }

    #[allow(dead_code)]
    pub fn window_downloaded(&self) -> u64 {
        self.download.window_bytes()
    }

    // ── Upload ───────────────────────────────────────────────────────────────

    #[allow(dead_code)]
    pub fn record_upload(&mut self, bytes: u64) {
        self.total_uploaded += bytes;
        self.upload.record(bytes);
        self.uploaded_in_last_round += bytes;
        self.uploaded_since_unchoked += bytes;
    }

    pub fn upload_rate_f64(&self) -> f64 {
        self.upload.rate()
    }

    #[allow(dead_code)]
    pub fn get_upload_rate(&self) -> u64 {
        self.upload.rate() as u64
    }

    #[allow(dead_code)]
    pub fn get_bytes_uploaded(&self) -> u64 {
        self.total_uploaded
    }

    #[allow(dead_code)]
    pub fn window_uploaded(&self) -> u64 {
        self.upload.window_bytes()
    }

    // ── Choking counters ─────────────────────────────────────────────────────

    #[allow(dead_code)]
    pub fn uploaded_in_last_round(&self) -> u64 {
        self.uploaded_in_last_round
    }

    #[allow(dead_code)]
    pub fn uploaded_since_unchoked(&self) -> u64 {
        self.uploaded_since_unchoked
    }

    /// Call at each unchoke interval.
    #[allow(dead_code)]
    pub fn reset_round_counters(&mut self) {
        self.uploaded_in_last_round = 0;
    }

    /// Call when peer transitions to choked.
    pub fn reset_since_unchoked(&mut self) {
        self.uploaded_since_unchoked = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    #[test]
    fn test_metrics_new() {
        let m = PeerMetrics::new();
        assert_eq!(m.get_download_rate(), 0);
        assert_eq!(m.get_bytes_downloaded(), 0);
    }

    #[test]
    fn test_record_download() {
        let mut m = PeerMetrics::new();
        m.record_download(1024);
        assert_eq!(m.get_bytes_downloaded(), 1024);
        assert_eq!(m.window_downloaded(), 1024);
    }

    #[test]
    fn test_rate_calculation() {
        let mut m = PeerMetrics::new();
        m.record_download(1024);
        thread::sleep(Duration::from_millis(100));
        m.update_rates();
        assert_eq!(m.get_bytes_downloaded(), 1024);
        assert!(m.download_rate_f64() >= 0.0);
    }

    #[test]
    fn test_rate_accuracy() {
        let mut m = PeerMetrics::new();
        for _ in 0..10 {
            m.record_download(102_400);
            thread::sleep(Duration::from_millis(100));
        }
        m.update_rates();
        let rate = m.get_download_rate();
        assert!((rate as i64 - 1_048_576).abs() < 104_857);
    }

    #[test]
    fn test_record_upload() {
        let mut m = PeerMetrics::new();
        m.record_upload(1024);
        assert_eq!(m.get_bytes_uploaded(), 1024);
        assert_eq!(m.window_uploaded(), 1024);
        assert_eq!(m.uploaded_in_last_round(), 1024);
        assert_eq!(m.uploaded_since_unchoked(), 1024);
    }

    #[test]
    fn test_upload_rate_calculation() {
        let mut m = PeerMetrics::new();
        m.record_upload(2048);
        thread::sleep(Duration::from_millis(100));
        m.update_rates();
        assert_eq!(m.get_bytes_uploaded(), 2048);
        assert!(m.upload_rate_f64() >= 0.0);
    }

    #[test]
    fn test_reset_round_counters() {
        let mut m = PeerMetrics::new();
        m.record_upload(1024);
        m.reset_round_counters();
        assert_eq!(m.uploaded_in_last_round(), 0);
        assert_eq!(m.uploaded_since_unchoked(), 1024);
        m.record_upload(512);
        assert_eq!(m.uploaded_in_last_round(), 512);
        assert_eq!(m.uploaded_since_unchoked(), 1536);
    }

    #[test]
    fn test_reset_since_unchoked() {
        let mut m = PeerMetrics::new();
        m.record_upload(1024);
        m.reset_since_unchoked();
        assert_eq!(m.uploaded_since_unchoked(), 0);
        assert_eq!(m.uploaded_in_last_round(), 1024);
    }

    #[test]
    fn test_upload_tracking_persists_across_rounds() {
        let mut m = PeerMetrics::new();
        m.record_upload(1000);
        m.reset_round_counters();
        m.record_upload(500);
        assert_eq!(m.uploaded_in_last_round(), 500);
        assert_eq!(m.uploaded_since_unchoked(), 1500);
        m.reset_round_counters();
        m.record_upload(200);
        assert_eq!(m.uploaded_in_last_round(), 200);
        assert_eq!(m.uploaded_since_unchoked(), 1700);
        m.reset_since_unchoked();
        assert_eq!(m.uploaded_since_unchoked(), 0);
        assert_eq!(m.uploaded_in_last_round(), 200);
    }

    #[test]
    fn test_upload_download_independent() {
        let mut m = PeerMetrics::new();
        m.record_download(1000);
        m.record_upload(500);
        assert_eq!(m.get_bytes_downloaded(), 1000);
        assert_eq!(m.get_bytes_uploaded(), 500);
        assert_eq!(m.window_downloaded(), 1000);
        assert_eq!(m.window_uploaded(), 500);
    }
}
