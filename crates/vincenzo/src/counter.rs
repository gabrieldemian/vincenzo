use std::sync::atomic::{AtomicU64, Ordering};

use tokio::{sync::Mutex, time::Instant};

/// Exponential Moving Average (EMA) smoothing factor
/// Higher values = more responsive to changes, lower values = smoother
const EMA_ALPHA: f64 = 0.3;

/// Counter of rates, used in downloaded and uploaded.
#[derive(Debug)]
pub struct Counter {
    // -- cumulative counters --
    pub total_downloaded: AtomicU64,
    pub total_uploaded: AtomicU64,

    // -- rate calculation --
    pub download_rate: AtomicU64,
    pub upload_rate: AtomicU64,

    // -- internal state --
    window_downloaded: AtomicU64,
    window_uploaded: AtomicU64,
    last_update: Mutex<Instant>,
    ema_download: Mutex<f64>,
    ema_upload: Mutex<f64>,
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            total_downloaded: AtomicU64::new(0),
            total_uploaded: AtomicU64::new(0),
            download_rate: AtomicU64::new(0),
            upload_rate: AtomicU64::new(0),
            window_downloaded: AtomicU64::new(0),
            window_uploaded: AtomicU64::new(0),
            last_update: Mutex::new(Instant::now()),
            ema_download: Mutex::new(0.0),
            ema_upload: Mutex::new(0.0),
        }
    }
}

impl Counter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record downloaded bytes
    pub fn record_download(&self, bytes: u64) {
        self.total_downloaded.fetch_add(bytes, Ordering::Relaxed);
        self.window_downloaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Record uploaded bytes
    pub fn record_upload(&self, bytes: u64) {
        self.total_uploaded.fetch_add(bytes, Ordering::Relaxed);
        self.window_uploaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Update rates with EMA smoothing
    pub async fn update_rates(&self) {
        let now = Instant::now();
        let mut last_update = self.last_update.lock().await;
        let elapsed = now.duration_since(*last_update).as_secs_f64();

        if elapsed < 0.001 {
            // Minimum 1ms elapsed
            return;
        }

        // Get and reset window counters
        let downloaded = self.window_downloaded.swap(0, Ordering::Relaxed);
        let uploaded = self.window_uploaded.swap(0, Ordering::Relaxed);

        // Calculate instantaneous rates
        let dl_rate = downloaded as f64 / elapsed;
        let ul_rate = uploaded as f64 / elapsed;

        // Apply EMA smoothing
        let mut ema_dl = self.ema_download.lock().await;
        let mut ema_ul = self.ema_upload.lock().await;

        *ema_dl = if *ema_dl == 0.0 {
            dl_rate
        } else {
            EMA_ALPHA * dl_rate + (1.0 - EMA_ALPHA) * *ema_dl
        };

        *ema_ul = if *ema_ul == 0.0 {
            ul_rate
        } else {
            EMA_ALPHA * ul_rate + (1.0 - EMA_ALPHA) * *ema_ul
        };

        // Store rates as integers (bytes/sec)
        self.download_rate.store(*ema_dl as u64, Ordering::Relaxed);
        self.upload_rate.store(*ema_ul as u64, Ordering::Relaxed);

        *last_update = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time;

    #[tokio::test]
    async fn test_counter_rates() {
        let counter = Counter::new();

        // Record initial downloads/uploads
        counter.record_download(1000);
        counter.record_upload(500);

        // First update: should set EMA to first instantaneous rate
        time::sleep(Duration::from_millis(100)).await;
        counter.update_rates().await;

        // Verify first EMA rates (≈10,000 DL, ≈5,000 UL)
        let dl1 = counter.download_rate.load(Ordering::Relaxed);
        let ul1 = counter.upload_rate.load(Ordering::Relaxed);
        assert!((9000..=11000).contains(&dl1)); // ≈10,000 ±10%
        assert!((4500..=5500).contains(&ul1)); // ≈5,000 ±10%

        // Second window: same data as first
        counter.record_download(1000);
        counter.record_upload(500);
        time::sleep(Duration::from_millis(100)).await;
        counter.update_rates().await;

        // EMA should stabilize near initial rate
        let dl2 = counter.download_rate.load(Ordering::Relaxed);
        let ul2 = counter.upload_rate.load(Ordering::Relaxed);
        assert!((dl2 as i64 - dl1 as i64).abs() < 1000); // <10% change
        assert!((ul2 as i64 - ul1 as i64).abs() < 500); // <10% change

        // Third window: double the data
        counter.record_download(2000);
        counter.record_upload(1000);
        time::sleep(Duration::from_millis(100)).await;
        counter.update_rates().await;

        // Verify EMA reacts to change (DL≈13,000, UL≈6,500)
        let dl3 = counter.download_rate.load(Ordering::Relaxed);
        let ul3 = counter.upload_rate.load(Ordering::Relaxed);
        assert!((11700..=14300).contains(&dl3)); // ≈13,000 ±10%
        assert!((5850..=7150).contains(&ul3)); // ≈6,500 ±10%
    }
}
