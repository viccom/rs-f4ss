use std::time::Duration;

// ---------------------------------------------------------------------------
// BandwidthEstimator — EMA of observed download throughput
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct BandwidthEstimator {
    ema_bytes_per_sec: f64,
    alpha: f64,
    initialized: bool,
}

// Start conservative: 5 MB/s
const INITIAL_BPS: f64 = 5_000_000.0;
const ALPHA: f64 = 0.3;

impl BandwidthEstimator {
    pub fn new() -> Self {
        Self {
            ema_bytes_per_sec: INITIAL_BPS,
            alpha: ALPHA,
            initialized: false,
        }
    }

    pub fn observe(&mut self, bytes: u64, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        if secs < 0.01 {
            return;
        }
        let throughput = bytes as f64 / secs;
        if !self.initialized {
            self.ema_bytes_per_sec = throughput;
            self.initialized = true;
        } else {
            self.ema_bytes_per_sec =
                self.alpha * throughput + (1.0 - self.alpha) * self.ema_bytes_per_sec;
        }
        tracing::debug!(
            "[bw] observe: {bytes} bytes in {secs:.2}s = {throughput:.0} B/s, ema={:.0} B/s ({:.1} MB/s)",
            self.ema_bytes_per_sec,
            self.ema_bytes_per_sec / 1_000_000.0,
        );
    }

    pub fn bandwidth_bps(&self) -> f64 {
        self.ema_bytes_per_sec
    }

    /// Calculate prefetch size: bandwidth × pipeline_seconds, clamped to [min, max].
    /// `remaining` is bytes left in the file from current offset.
    pub fn prefetch_size(&self, pipeline_seconds: f64, remaining: u64) -> u32 {
        let ideal = self.ema_bytes_per_sec * pipeline_seconds;
        let min: f64 = 256.0 * 1024.0;
        let max: f64 = 16.0 * 1024.0 * 1024.0; // 16 MB — large enough for smooth playback, small enough to avoid timeouts
        let cap = remaining as f64;
        let upper = max.min(cap).max(min); // ensure upper >= min
        let size = ideal.clamp(min, upper);
        size as u32
    }
}

// ---------------------------------------------------------------------------
// ReadPattern — per-handle sequential access detection
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ReadPattern {
    pub last_read_end: u64,
    pub sequential_count: u32,
    pub is_sequential: bool,
}

impl ReadPattern {
    pub fn new() -> Self {
        Self {
            last_read_end: 0,
            sequential_count: 0,
            is_sequential: false,
        }
    }

    /// Record a read and return whether it looks sequential.
    pub fn update(&mut self, offset: u64, size: u32) -> bool {
        if self.last_read_end == 0 && offset == 0 {
            // First read from file start
            self.sequential_count = 1;
        } else if offset == self.last_read_end {
            self.sequential_count = self.sequential_count.saturating_add(1);
        } else {
            self.sequential_count = 0;
        }
        self.last_read_end = offset.saturating_add(size as u64);
        self.is_sequential = self.sequential_count >= 2;
        self.is_sequential
    }

    /// True if this is the very first read on this handle.
    pub fn is_first_read(&self) -> bool {
        self.sequential_count <= 1 && self.last_read_end == 0
            || self.sequential_count == 1 && self.last_read_end > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bandwidth_initial() {
        let bw = BandwidthEstimator::new();
        assert_eq!(bw.bandwidth_bps(), INITIAL_BPS);
    }

    #[test]
    fn test_bandwidth_observe() {
        let mut bw = BandwidthEstimator::new();
        bw.observe(10_000_000, Duration::from_secs(1));
        assert!(bw.bandwidth_bps() > 0.0);
        // After first observation, EMA should match it exactly
        assert!((bw.bandwidth_bps() - 10_000_000.0).abs() < 1.0);
    }

    #[test]
    fn test_bandwidth_ema() {
        let mut bw = BandwidthEstimator::new();
        bw.observe(10_000_000, Duration::from_secs(1)); // 10 MB/s
        bw.observe(20_000_000, Duration::from_secs(1)); // 20 MB/s
                                                        // EMA = 0.3 * 20M + 0.7 * 10M = 6M + 7M = 13M
        assert!((bw.bandwidth_bps() - 13_000_000.0).abs() < 1.0);
    }

    #[test]
    fn test_bandwidth_short_ignore() {
        let mut bw = BandwidthEstimator::new();
        bw.observe(100, Duration::from_millis(1)); // Too short
        assert!(!bw.initialized);
        assert_eq!(bw.bandwidth_bps(), INITIAL_BPS);
    }

    #[test]
    fn test_prefetch_size_clamp() {
        let bw = BandwidthEstimator::new();
        // With initial 5 MB/s and 5s pipeline: 25 MB, clamped by remaining
        // When remaining is very small, minimum of 256KB applies
        let size = bw.prefetch_size(5.0, 1024);
        assert_eq!(size, 256 * 1024); // min is 256KB
    }

    #[test]
    fn test_read_pattern_sequential() {
        let mut p = ReadPattern::new();
        assert!(!p.update(0, 1024)); // first read from 0 → count=1, is_seq=false
        assert!(!p.is_sequential);
        assert!(p.update(1024, 1024)); // continues → count=2, is_seq=true
        assert!(p.is_sequential);
    }

    #[test]
    fn test_read_pattern_random() {
        let mut p = ReadPattern::new();
        p.update(0, 1024);
        p.update(50000, 1024); // non-sequential
        assert!(!p.is_sequential);
        assert_eq!(p.sequential_count, 0);
    }
}
