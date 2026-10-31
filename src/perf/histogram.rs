//! Fixed-size frame-time histogram.
//!
//! Requirements that shaped this type:
//!
//! * `record()` must be O(1), lock-free and allocation-free — it runs inside the
//!   frame path.
//! * P50/P95/P99 must be exact to 1 µs over the range that matters (0 … 16 ms),
//!   with everything above that bucketed into a single overflow slot.
//!
//! Bucket counting gives all three: 16 384 buckets × 4 bytes = 64 KiB per
//! histogram, six histograms per renderer at most.

use serde::Serialize;

/// Number of 1 µs buckets; the last bucket is the overflow slot.
pub const BUCKETS: usize = 16_384;

/// Immutable view of a histogram, embedded in the capabilities/perf JSON.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct HistogramSnapshot {
    /// Samples recorded.
    pub count: u64,
    /// Arithmetic mean in microseconds.
    pub mean_us: f64,
    /// 50th percentile in microseconds.
    pub p50_us: u32,
    /// 95th percentile in microseconds.
    pub p95_us: u32,
    /// 99th percentile in microseconds.
    pub p99_us: u32,
    /// Worst sample in microseconds.
    pub max_us: u32,
}

/// Bucketed frame-time histogram.
pub struct FrameHistogram {
    buckets: Box<[u32]>,
    count: u64,
    sum_us: u64,
    max_us: u32,
}

impl FrameHistogram {
    /// Allocate the bucket array (init-time only).
    pub fn new() -> FrameHistogram {
        FrameHistogram {
            buckets: vec![0u32; BUCKETS].into_boxed_slice(),
            count: 0,
            sum_us: 0,
            max_us: 0,
        }
    }

    /// Record one sample in microseconds. Allocation-free.
    pub fn record(&mut self, micros: u32) {
        let index = (micros as usize).min(BUCKETS - 1);
        self.buckets[index] = self.buckets[index].saturating_add(1);
        self.count += 1;
        self.sum_us += u64::from(micros);
        if micros > self.max_us {
            self.max_us = micros;
        }
    }

    /// Forget every sample, keeping the allocation.
    pub fn reset(&mut self) {
        for bucket in self.buckets.iter_mut() {
            *bucket = 0;
        }
        self.count = 0;
        self.sum_us = 0;
        self.max_us = 0;
    }

    /// Samples recorded since the last reset.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Worst sample in microseconds.
    pub fn max_us(&self) -> u32 {
        self.max_us
    }

    /// Percentile in `0.0 ..= 100.0` microseconds (0 when empty).
    pub fn percentile(&self, percent: f64) -> u32 {
        if self.count == 0 {
            return 0;
        }
        let clamped = percent.clamp(0.0, 100.0);
        let target = ((clamped / 100.0) * self.count as f64).ceil() as u64;
        let target = target.max(1);
        let mut seen = 0u64;
        for (index, bucket) in self.buckets.iter().enumerate() {
            seen += u64::from(*bucket);
            if seen >= target {
                return index as u32;
            }
        }
        self.max_us
    }

    /// Snapshot for JSON.
    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            count: self.count,
            mean_us: if self.count == 0 {
                0.0
            } else {
                self.sum_us as f64 / self.count as f64
            },
            p50_us: self.percentile(50.0),
            p95_us: self.percentile(95.0),
            p99_us: self.percentile(99.0),
            max_us: self.max_us,
        }
    }
}

impl Default for FrameHistogram {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_histogram_is_zero() {
        let hist = FrameHistogram::new();
        let snap = hist.snapshot();
        assert_eq!(snap.count, 0);
        assert_eq!(snap.p50_us, 0);
        assert_eq!(snap.p99_us, 0);
        assert_eq!(snap.mean_us, 0.0);
    }

    #[test]
    fn percentiles_are_exact_for_uniform_input() {
        let mut hist = FrameHistogram::new();
        for us in 1..=1000u32 {
            hist.record(us);
        }
        let snap = hist.snapshot();
        assert_eq!(snap.count, 1000);
        assert_eq!(snap.p50_us, 500);
        assert_eq!(snap.p95_us, 950);
        assert_eq!(snap.p99_us, 990);
        assert_eq!(snap.max_us, 1000);
        assert!((snap.mean_us - 500.5).abs() < 0.01);
    }

    #[test]
    fn overflow_bucket_pins_max() {
        let mut hist = FrameHistogram::new();
        hist.record(1);
        hist.record(1_000_000);
        assert_eq!(hist.max_us(), 1_000_000);
        assert_eq!(hist.percentile(100.0), BUCKETS as u32 - 1);
    }

    #[test]
    fn reset_clears_samples_but_keeps_capacity() {
        let mut hist = FrameHistogram::new();
        hist.record(42);
        hist.reset();
        assert_eq!(hist.count(), 0);
        assert_eq!(hist.max_us(), 0);
        hist.record(7);
        assert_eq!(hist.percentile(50.0), 7);
    }
}
