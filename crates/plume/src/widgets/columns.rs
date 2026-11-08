//! Per-device-pixel column data for the GL graph renderer.
//!
//! Column `c` represents time `c · dt` (µs on the monotonic clock). Its value
//! is the curve at that instant (linear or clamped Catmull-Rom interpolation
//! between samples) or, when several samples fall inside one column (long
//! time scales), the largest of them, so spikes never disappear. Columns past
//! the newest sample hold its value until the next sample arrives. Columns
//! live in a ring the size of the GPU texture (`slot = c mod texw`); only the
//! columns touched by new samples are recomputed and re-uploaded.

pub const NO_DATA: f32 = -1.0;

#[derive(Default)]
pub struct Columns {
    pub texw: usize,
    pub n: usize,
    /// µs per column.
    pub dt: f64,
    pub data: Vec<[f32; 4]>,
    /// Per column: [min, max] of series 0 and 1 over the shader's 6-vertex
    /// window (columns c-2..=c+3). Lets most pixels skip the exact test.
    pub env: Vec<[f32; 4]>,
    /// Columns below this are final (both neighbouring samples known).
    pub final_until: i64,
    /// Inclusive range of columns changed since the last upload.
    pub dirty: Option<(i64, i64)>,
    /// The whole ring must be uploaded (new size / reset).
    pub full: bool,
}

impl Columns {
    pub fn reset(&mut self, texw: usize, dt: f64, n: usize) {
        self.texw = texw.max(2);
        self.dt = dt;
        self.n = n.min(4);
        self.data.clear();
        self.data.resize(self.texw, [NO_DATA; 4]);
        self.env.clear();
        self.env.resize(self.texw, [NO_DATA; 4]);
        self.final_until = i64::MIN;
        self.dirty = None;
        self.full = true;
    }

    #[inline]
    pub fn slot(&self, c: i64) -> usize {
        c.rem_euclid(self.texw as i64) as usize
    }

    fn mark(&mut self, lo: i64, hi: i64) {
        if hi < lo {
            return;
        }
        self.dirty = Some(match self.dirty {
            Some((a, b)) => (a.min(lo), b.max(hi)),
            None => (lo, hi),
        });
    }

    /// Recomputes columns `c_from..=c_to` of series `s` from `pts` (sorted by
    /// time; must reach back at least one sample before `c_from`).
    pub fn update_series(&mut self, s: usize, pts: &[(i64, f32)], c_from: i64, c_to: i64, smooth: bool) {
        if s >= self.n || c_to < c_from {
            return;
        }
        // Never write more than one ring's worth (older columns would be overwritten anyway).
        let c_from = c_from.max(c_to - self.texw as i64 + 1);
        let dt = self.dt;
        let half = dt * 0.5;
        let n = pts.len();
        let (mut lo, mut hi) = (0usize, 0usize);
        for c in c_from..=c_to {
            let tc = c as f64 * dt;
            let v = if n == 0 {
                NO_DATA
            } else {
                // Largest sample inside this column's time span.
                while lo < n && (pts[lo].0 as f64) < tc - half {
                    lo += 1;
                }
                let mut mx = NO_DATA;
                let mut k = lo;
                while k < n && (pts[k].0 as f64) < tc + half {
                    mx = mx.max(pts[k].1);
                    k += 1;
                }
                // Curve value at the column centre: pts[hi-1].t <= tc < pts[hi].t
                while hi < n && (pts[hi].0 as f64) <= tc {
                    hi += 1;
                }
                let interp = if hi == 0 {
                    NO_DATA
                } else if hi == n {
                    pts[n - 1].1 // live edge: hold the newest value
                } else {
                    let (t1, v1) = pts[hi - 1];
                    let (t2, v2) = pts[hi];
                    let f = ((tc - t1 as f64) / (t2 - t1).max(1) as f64) as f32;
                    if smooth {
                        let v0 = if hi >= 2 { pts[hi - 2].1 } else { v1 };
                        let v3 = if hi + 1 < n { pts[hi + 1].1 } else { v2 };
                        catmull_clamped(v0, v1, v2, v3, f)
                    } else {
                        v1 + (v2 - v1) * f
                    }
                };
                interp.max(mx)
            };
            let slot = self.slot(c);
            self.data[slot][s] = v;
        }
        self.mark(c_from, c_to);
    }

    /// Recomputes the envelope for every column whose 6-vertex window
    /// overlaps `lo..=hi` (call after updating series data).
    pub fn update_env(&mut self, lo: i64, hi: i64) {
        // Empty when the display time stands still while data keeps coming.
        if hi < lo {
            return;
        }
        let (lo, hi) = (lo - 3, hi + 2);
        let lo = lo.max(hi - self.texw as i64 + 1);
        for c in lo..=hi {
            let mut e = [NO_DATA; 4];
            for s in 0..self.n.min(2) {
                let (mut mn, mut mx) = (f32::INFINITY, NO_DATA);
                for j in c - 2..=c + 3 {
                    let v = self.data[self.slot(j)][s];
                    if v >= 0.0 {
                        mn = mn.min(v);
                        mx = mx.max(v);
                    }
                }
                if mx >= 0.0 {
                    e[s * 2] = mn;
                    e[s * 2 + 1] = mx;
                }
            }
            let slot = self.slot(c);
            self.env[slot] = e;
        }
        self.mark(lo, hi);
    }

    /// Largest value of any series over columns `lo..=hi`.
    pub fn max_in(&self, lo: i64, hi: i64) -> f32 {
        let lo = lo.max(hi - self.texw as i64 + 1);
        let mut m = 0.0f32;
        for c in lo..=hi {
            let d = &self.data[self.slot(c)];
            for v in &d[..self.n] {
                m = m.max(*v);
            }
        }
        m
    }

    /// Slot ranges to upload for the dirty columns: (first slot, length).
    /// Upload `data[slot..slot+len]` to row 0 and `env[..]` to row 1.
    pub fn take_dirty(&mut self) -> Vec<(usize, usize)> {
        let Some((lo, hi)) = self.dirty.take() else { return Vec::new() };
        if hi < lo {
            return Vec::new();
        }
        let lo = lo.max(hi - self.texw as i64 + 1);
        let len = (hi - lo + 1) as usize;
        let s0 = self.slot(lo);
        if s0 + len <= self.texw {
            vec![(s0, len)]
        } else {
            let first = self.texw - s0;
            vec![(s0, first), (0, len - first)]
        }
    }
}

/// Catmull-Rom between v1 and v2, clamped to their range so it never overshoots.
fn catmull_clamped(v0: f32, v1: f32, v2: f32, v3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    let v = 0.5
        * ((2.0 * v1)
            + (-v0 + v2) * t
            + (2.0 * v0 - 5.0 * v1 + 4.0 * v2 - v3) * t2
            + (-v0 + 3.0 * v1 - 3.0 * v2 + v3) * t3);
    v.clamp(v1.min(v2), v1.max(v2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_holds_and_keeps_peaks() {
        let mut c = Columns::default();
        // 10 columns per sample: dt = 10 ms, samples every 100 ms.
        c.reset(64, 10_000.0, 1);
        let pts = [(0i64, 0.0f32), (100_000, 1.0), (200_000, 0.0)];
        c.update_series(0, &pts, 0, 30, false);
        assert_eq!(c.data[c.slot(0)][0], 0.0);
        assert!((c.data[c.slot(5)][0] - 0.5).abs() < 1e-6, "midpoint interpolated");
        assert_eq!(c.data[c.slot(10)][0], 1.0);
        assert_eq!(c.data[c.slot(30)][0], 0.0, "holds the newest value past the last sample");
        assert_eq!(c.data[c.slot(-3)][0], NO_DATA, "untouched columns stay empty");
        assert_eq!(c.take_dirty().iter().map(|(_, n)| n).sum::<usize>(), 31);

        // Long time scale: 1 column = 1 s, samples every 100 ms with one spike.
        let mut c = Columns::default();
        c.reset(16, 1_000_000.0, 1);
        let pts: Vec<(i64, f32)> = (0..50).map(|i| (i * 100_000, if i == 23 { 9.0 } else { 1.0 })).collect();
        c.update_series(0, &pts, 0, 4, false);
        assert_eq!(c.data[c.slot(2)][0], 9.0, "spike inside a column survives");
        assert_eq!(c.max_in(0, 4), 9.0);
    }

    #[test]
    fn ring_upload_wraps() {
        let mut c = Columns::default();
        c.reset(8, 1.0, 1);
        c.update_series(0, &[(0, 1.0)], 5, 10, false);
        let parts = c.take_dirty();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, 5);
        assert_eq!(parts[0].1 + parts[1].1, 6);
    }

    #[test]
    fn envelope_spans_shader_window() {
        let mut c = Columns::default();
        c.reset(32, 1.0, 1);
        let pts: Vec<(i64, f32)> = (0..20).map(|i| (i, if i == 10 { 5.0 } else { 1.0 })).collect();
        c.update_series(0, &pts, 0, 19, false);
        c.update_env(0, 19);
        // Column 10's spike is inside the windows of columns 7..=12.
        for col in 7..=12 {
            assert_eq!(c.env[c.slot(col)][1], 5.0, "col {col}");
        }
        assert_eq!(c.env[c.slot(6)][1], 1.0);
        assert_eq!(c.env[c.slot(13)][1], 1.0);
        assert_eq!(c.env[c.slot(10)][0], 1.0, "min");
    }

    #[test]
    fn smooth_never_overshoots() {
        for i in 0..=10 {
            let v = catmull_clamped(0.0, 0.0, 1.0, 1.0, i as f32 / 10.0);
            assert!((0.0..=1.0).contains(&v));
        }
    }
    #[test]
    fn empty_ranges_mark_nothing() {
        // Display time frozen, data still arriving: the update range is empty.
        let mut c = Columns::default();
        c.reset(64, 1.0, 1);
        c.take_dirty();
        c.update_series(0, &[(0, 0.5), (100, 0.5)], 50, 40, false);
        c.update_env(50, 40);
        assert!(c.take_dirty().is_empty());
    }

}
