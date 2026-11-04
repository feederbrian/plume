//! Fixed-capacity time series shared with the UI.
//!
//! Two tiers per series:
//! * **fine**: every sample (10 Hz by default), ~5 minutes;
//! * **coarse**: 1-second means, 1 hour.
//!
//! `copy_since` stitches them together, so a graph can show anything from
//! 30 seconds to an hour with no gap at the live edge.

/// Fine-tier points. At 10 Hz this is ~5 minutes; 36 KiB per series.
pub const CAPACITY: usize = 3072;
/// Coarse-tier points (1 s buckets): one hour; 43 KiB per series.
pub const COARSE_CAPACITY: usize = 3600;
const BUCKET_US: i64 = 1_000_000;

#[derive(Clone)]
struct Ring {
    t: Box<[i64]>,
    v: Box<[f32]>,
    head: usize,
    len: usize,
}

impl Ring {
    fn new(cap: usize) -> Ring {
        Ring { t: vec![0; cap].into_boxed_slice(), v: vec![0.0; cap].into_boxed_slice(), head: 0, len: 0 }
    }
    #[inline]
    fn cap(&self) -> usize {
        self.t.len()
    }
    #[inline]
    fn push(&mut self, t: i64, v: f32) {
        let cap = self.cap();
        self.t[self.head] = t;
        self.v[self.head] = v;
        self.head = (self.head + 1) % cap;
        self.len = (self.len + 1).min(cap);
    }
    #[inline]
    fn get(&self, i: usize) -> (i64, f32) {
        let cap = self.cap();
        let k = (self.head + cap - self.len + i) % cap;
        (self.t[k], self.v[k])
    }
    /// First logical index with t >= since.
    fn lower_bound(&self, since: i64) -> usize {
        let (mut lo, mut hi) = (0usize, self.len);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.get(mid).0 < since {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

#[derive(Clone)]
pub struct Series {
    fine: Ring,
    coarse: Ring,
    acc_sum: f64,
    acc_n: u32,
    acc_start: i64,
    acc_last_t: i64,
    /// Bumped on every push; lets the UI skip work when nothing changed.
    pub gen: u64,
}

impl Default for Series {
    fn default() -> Self {
        Self::new()
    }
}

impl Series {
    pub fn new() -> Series {
        Series {
            fine: Ring::new(CAPACITY),
            coarse: Ring::new(COARSE_CAPACITY),
            acc_sum: 0.0,
            acc_n: 0,
            acc_start: 0,
            acc_last_t: 0,
            gen: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, t_us: i64, v: f32) {
        self.fine.push(t_us, v);
        if self.acc_n > 0 && t_us - self.acc_start >= BUCKET_US {
            // Close the 1 s bucket, timestamped at its newest sample.
            self.coarse.push(self.acc_last_t, (self.acc_sum / self.acc_n as f64) as f32);
            self.acc_sum = 0.0;
            self.acc_n = 0;
        }
        if self.acc_n == 0 {
            self.acc_start = t_us;
        }
        self.acc_sum += v as f64;
        self.acc_n += 1;
        self.acc_last_t = t_us;
        self.gen += 1;
    }

    /// Fine-tier length.
    pub fn len(&self) -> usize {
        self.fine.len
    }

    pub fn is_empty(&self) -> bool {
        self.fine.len == 0
    }

    /// i-th oldest fine point.
    pub fn get(&self, i: usize) -> (i64, f32) {
        self.fine.get(i)
    }

    pub fn last(&self) -> Option<(i64, f32)> {
        (self.fine.len > 0).then(|| self.fine.get(self.fine.len - 1))
    }

    /// Average spacing of the last few samples, in µs.
    pub fn interval_us(&self) -> Option<i64> {
        let n = self.fine.len.min(8);
        if n < 2 {
            return None;
        }
        let (a, _) = self.fine.get(self.fine.len - n);
        let (b, _) = self.fine.get(self.fine.len - 1);
        Some((b - a) / (n as i64 - 1))
    }

    /// Copies points with `t >= since` (plus the one just before, so a line
    /// can enter from the left edge) into `out`, oldest first. Uses fine
    /// samples where they exist and 1-second means beyond that.
    pub fn copy_since(&self, since: i64, out: &mut Vec<(i64, f32)>) {
        out.clear();
        if self.fine.len == 0 {
            return;
        }
        let fine_oldest = self.fine.get(0).0;
        if since < fine_oldest && self.coarse.len > 0 {
            // Older part from the coarse tier, up to where fine data starts.
            let start = self.coarse.lower_bound(since).saturating_sub(1);
            for i in start..self.coarse.len {
                let p = self.coarse.get(i);
                if p.0 >= fine_oldest {
                    break;
                }
                out.push(p);
            }
            for i in 0..self.fine.len {
                out.push(self.fine.get(i));
            }
            return;
        }
        let start = self.fine.lower_bound(since).saturating_sub(1);
        out.reserve(self.fine.len - start);
        for i in start..self.fine.len {
            out.push(self.fine.get(i));
        }
    }

    /// Mean of the samples in the last `window_us` (ending at the newest
    /// sample). Labels use this so they agree with 1 s per-process figures.
    pub fn mean_recent(&self, window_us: i64) -> Option<f32> {
        let (t_last, _) = self.last()?;
        let (mut sum, mut n) = (0.0f64, 0u32);
        for i in (0..self.fine.len).rev() {
            let (t, v) = self.fine.get(i);
            if t <= t_last - window_us {
                break;
            }
            sum += v as f64;
            n += 1;
        }
        (n > 0).then(|| (sum / n as f64) as f32)
    }

    /// Max value over fine points with `t >= since`.
    pub fn max_since(&self, since: i64) -> f32 {
        let mut m = 0.0f32;
        for i in (0..self.fine.len).rev() {
            let (t, v) = self.fine.get(i);
            if t < since {
                break;
            }
            m = m.max(v);
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wraps_and_searches() {
        let mut s = Series::new();
        for i in 0..(CAPACITY + 10) {
            s.push(i as i64 * 100, i as f32);
        }
        assert_eq!(s.len(), CAPACITY);
        assert_eq!(s.get(0).1, 10.0);
        assert_eq!(s.last().unwrap().1, (CAPACITY + 9) as f32);
        assert_eq!(s.interval_us(), Some(100));
        let mut out = Vec::new();
        let since = (CAPACITY as i64 + 5) * 100;
        s.copy_since(since, &mut out);
        assert_eq!(out.first().unwrap().0, since - 100, "includes one point before");
        assert_eq!(out.len(), 6);
        assert_eq!(s.max_since(since), (CAPACITY + 9) as f32);
        // last 5 points (t > last - 500): values C+5..=C+9 → mean C+7
        assert_eq!(s.mean_recent(500), Some((CAPACITY + 7) as f32));
    }

    #[test]
    fn long_windows_stitch_coarse_and_fine() {
        // 20 minutes at 10 Hz: fine keeps the last ~5 min, coarse the rest.
        let mut s = Series::new();
        let n = 20 * 60 * 10;
        for i in 0..n {
            s.push(i as i64 * 100_000, (i % 10) as f32);
        }
        let now = (n as i64 - 1) * 100_000;
        let mut out = Vec::new();
        s.copy_since(now - 15 * 60 * 1_000_000, &mut out);
        // ~10 min of coarse (1 Hz) + ~5 min of fine (10 Hz)
        assert!(out.len() > 590 + CAPACITY - 10, "{}", out.len());
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0), "strictly increasing time");
        assert!(out.first().unwrap().0 <= now - 15 * 60 * 1_000_000);
        // Coarse means of 0..9 are 4.5.
        assert!((out[5].1 - 4.5).abs() < 1e-3, "{}", out[5].1);
        assert_eq!(out.last().unwrap().0, now);
    }
}
