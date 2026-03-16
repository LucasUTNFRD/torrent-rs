use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Exponential moving average of a byte rate (bytes/sec).
///
/// Call `record` whenever bytes are transferred.
/// Call `update` on a periodic tick (≥500ms cadence recommended) to flush
/// the window and recompute the EMA.
#[derive(Debug, Clone)]
pub struct EmaRate {
    alpha: f64,
    window_bytes: u64,
    ema: f64,
    last_update_ms: u64,
}

impl EmaRate {
    /// `alpha` is the EMA smoothing factor (0 < alpha < 1).
    /// Higher → more reactive; lower → smoother.
    /// Typical value: 0.3.
    pub fn new(alpha: f64) -> Self {
        debug_assert!(alpha > 0.0 && alpha < 1.0, "alpha must be in (0, 1)");
        Self {
            alpha,
            window_bytes: 0,
            ema: 0.0,
            last_update_ms: now_ms(),
        }
    }

    /// Accumulate bytes into the current window.
    #[inline]
    pub fn record(&mut self, bytes: u64) {
        self.window_bytes += bytes;
    }

    /// Flush the window and update the EMA.
    ///
    /// Returns the updated rate in bytes/sec.
    /// No-ops (returns current rate) if less than 500ms have elapsed.
    pub fn update(&mut self) -> f64 {
        let now = now_ms();
        let elapsed_ms = now.saturating_sub(self.last_update_ms);

        if elapsed_ms < 500 {
            return self.ema;
        }

        let elapsed_secs = elapsed_ms as f64 / 1000.0;
        let instant_rate = self.window_bytes as f64 / elapsed_secs;

        self.window_bytes = 0;
        self.last_update_ms = now;

        self.ema = if self.ema == 0.0 {
            instant_rate
        } else {
            self.alpha * instant_rate + (1.0 - self.alpha) * self.ema
        };

        self.ema
    }

    /// Current EMA rate in bytes/sec without triggering an update.
    #[inline]
    pub fn rate(&self) -> f64 {
        self.ema
    }

    /// Bytes accumulated in the current window (not yet flushed).
    #[inline]
    pub fn window_bytes(&self) -> u64 {
        self.window_bytes
    }

    /// Reset window and timestamp. Use when a peer slot is reused.
    pub fn reset(&mut self) {
        self.window_bytes = 0;
        self.ema = 0.0;
        self.last_update_ms = now_ms();
    }
}

impl Default for EmaRate {
    fn default() -> Self {
        Self::new(0.3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    #[test]
    fn initial_state() {
        let r = EmaRate::default();
        assert_eq!(r.rate(), 0.0);
        assert_eq!(r.window_bytes(), 0);
    }

    #[test]
    fn record_accumulates_window() {
        let mut r = EmaRate::default();
        r.record(1024);
        r.record(1024);
        assert_eq!(r.window_bytes(), 2048);
    }

    #[test]
    fn update_debounce_under_500ms() {
        let mut r = EmaRate::default();
        r.record(1_000_000);
        // immediately — should no-op
        let rate = r.update();
        assert_eq!(rate, 0.0);
        // window not flushed
        assert_eq!(r.window_bytes(), 1_000_000);
    }

    #[test]
    fn update_flushes_window_after_500ms() {
        let mut r = EmaRate::default();
        r.record(102_400);
        thread::sleep(Duration::from_millis(600));
        let rate = r.update();
        assert!(rate > 0.0, "rate should be positive after flush");
        assert_eq!(r.window_bytes(), 0, "window should be cleared");
    }

    #[test]
    fn reset_clears_state() {
        let mut r = EmaRate::default();
        r.record(1024);
        thread::sleep(Duration::from_millis(600));
        r.update();
        assert!(r.rate() > 0.0);
        r.reset();
        assert_eq!(r.rate(), 0.0);
        assert_eq!(r.window_bytes(), 0);
    }

    #[test]
    fn ema_smooths_across_ticks() {
        let mut r = EmaRate::new(0.3);
        // First tick: seed EMA
        r.record(10_000);
        thread::sleep(Duration::from_millis(600));
        let r1 = r.update();
        // Second tick: double the rate
        r.record(20_000);
        thread::sleep(Duration::from_millis(600));
        let r2 = r.update();
        // EMA should be between r1 and the instant rate of r2's window
        assert!(r2 > r1, "EMA should increase when throughput doubles");
    }

    #[test]
    fn custom_alpha() {
        // High alpha → rate tracks instant rate more aggressively
        let mut fast = EmaRate::new(0.9);
        // Low alpha → rate changes slowly
        let mut slow = EmaRate::new(0.1);

        for r in [&mut fast, &mut slow] {
            r.record(50_000);
            thread::sleep(Duration::from_millis(600));
            r.update();
        }

        // Both seeded from zero so first tick sets EMA = instant_rate,
        // meaning they're equal after the first update (no prior EMA to blend).
        // Record a very different amount for the second tick.
        fast.record(1_000_000);
        slow.record(1_000_000);
        thread::sleep(Duration::from_millis(600));
        let fast_rate = fast.update();
        let slow_rate = slow.update();

        assert!(
            fast_rate > slow_rate,
            "higher alpha should track the spike more aggressively"
        );
    }
}
