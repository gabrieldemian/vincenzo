use crate::utils::to_human_readable;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Exponential Moving Average (EMA) smoothing factor
/// Higher values = more responsive to changes, lower values = smoother
static EMA_ALPHA: f64 = 0.3;

static EMA_ALPHA_COMPLIMENT: f64 = 0.7; // 1.0 - 0.3

/// Counter of rates, used in downloaded and uploaded.
#[derive(Debug)]
pub struct Counter {
    total_downloaded: AtomicU64,
    total_uploaded: AtomicU64,

    window_downloaded: AtomicU64,
    window_uploaded: AtomicU64,

    ema_download: AtomicU64,
    ema_upload: AtomicU64,

    last_update: AtomicU64,
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            total_downloaded: AtomicU64::new(0),
            total_uploaded: AtomicU64::new(0),
            window_downloaded: AtomicU64::new(0),
            window_uploaded: AtomicU64::new(0),
            last_update: AtomicU64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            ),
            ema_download: AtomicU64::new(0.0f64.to_bits()),
            ema_upload: AtomicU64::new(0.0f64.to_bits()),
        }
    }
}

impl Counter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_total_download(t: u64) -> Self {
        Counter { total_downloaded: t.into(), ..Default::default() }
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

    pub fn download_rate_f64(&self) -> f64 {
        let bits = self.ema_download.load(Ordering::Relaxed);
        f64::from_bits(bits)
    }

    pub fn upload_rate_f64(&self) -> f64 {
        let bits = self.ema_upload.load(Ordering::Relaxed);
        f64::from_bits(bits)
    }

    pub fn window_uploaded_u64(&self) -> u64 {
        self.window_uploaded.load(Ordering::Relaxed)
    }

    pub fn window_downloaded_u64(&self) -> u64 {
        self.window_downloaded.load(Ordering::Relaxed)
    }

    pub fn download_rate_u64(&self) -> u64 {
        self.ema_download.load(Ordering::Relaxed)
    }

    pub fn total_download(&self) -> u64 {
        self.total_downloaded.load(Ordering::Relaxed)
    }

    pub fn total_upload(&self) -> u64 {
        self.total_uploaded.load(Ordering::Relaxed)
    }

    pub fn upload_rate_u64(&self) -> u64 {
        self.ema_upload.load(Ordering::Relaxed)
    }

    pub fn download_rate(&self) -> String {
        let mut v = to_human_readable(self.download_rate_f64());
        v.push_str("/s");
        v
    }

    pub fn upload_rate(&self) -> String {
        let mut v = to_human_readable(self.upload_rate_f64());
        v.push_str("/s");
        v
    }

    /// Update rates with EMA smoothing
    pub fn update_rates(&self) {
        let now =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
                as u64;

        let last = self.last_update.load(Ordering::Acquire);
        let elapsed_ms = now - last;

        if elapsed_ms < 1 {
            return;
        }

        let elapsed = elapsed_ms as f64 / 1000.0;
        self.last_update.store(now, Ordering::Release);

        let downloaded = self.window_downloaded.swap(0, Ordering::Relaxed);
        let uploaded = self.window_uploaded.swap(0, Ordering::Relaxed);

        let dl_rate = downloaded as f64 / elapsed;
        let ul_rate = uploaded as f64 / elapsed;

        let ema_dl_bits = self.ema_download.load(Ordering::Relaxed);
        let current_ema_dl = f64::from_bits(ema_dl_bits);

        let new_ema_dl = if current_ema_dl == 0.0 {
            dl_rate
        } else {
            EMA_ALPHA * dl_rate + EMA_ALPHA_COMPLIMENT * current_ema_dl
        };

        self.ema_download.store(new_ema_dl.to_bits(), Ordering::Release);

        let ema_ul_bits = self.ema_upload.load(Ordering::Relaxed);
        let current_ema_ul = f64::from_bits(ema_ul_bits);

        let new_ema_ul = if current_ema_ul == 0.0 {
            ul_rate
        } else {
            EMA_ALPHA * ul_rate + EMA_ALPHA_COMPLIMENT * current_ema_ul
        };

        self.ema_upload.store(new_ema_ul.to_bits(), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_download_rate_f64() {
        let counter = Counter::default();

        counter.ema_download.store(1234.56f64.to_bits(), Ordering::Relaxed);
        assert!((counter.download_rate_f64() - 1234.56).abs() < f64::EPSILON);

        counter.ema_download.store(0.0f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate_f64(), 0.0);
    }

    #[test]
    fn total_counters() {
        let counter = Counter::new();

        counter.record_download(1000);
        counter.record_upload(500);
        counter.record_download(2000);
        counter.record_upload(1000);
        counter.record_download(1);

        assert_eq!(counter.total_download(), 3001);
        assert_eq!(counter.total_upload(), 1500);

        counter.update_rates();

        assert_eq!(counter.total_download(), 3001);
        assert_eq!(counter.total_upload(), 1500);
    }

    #[test]
    fn format_rate() {
        let counter = Counter::default();

        counter.ema_download.store(500.000f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate(), "500 B/s");

        counter.ema_download.store(1500.0f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate(), "1.5 KB/s");

        counter.ema_download.store(2_380_000.0f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate(), "2.38 MB/s");

        counter
            .ema_download
            .store(2_330_000_000.0f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate(), "2.33 GB/s");

        counter.ema_download.store(1000.0f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate(), "1 KB/s");

        counter.ema_download.store(0.0f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate(), "0 B/s");

        counter.ema_download.store(0.9f64.to_bits(), Ordering::Relaxed);
        assert_eq!(counter.download_rate(), "0.9 B/s");
    }
}
