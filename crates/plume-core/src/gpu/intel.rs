//! Intel integrated graphics (i915 and xe drivers) without root.
//!
//! btop and intel_gpu_top read engine busyness through the i915 perf PMU,
//! which needs CAP_PERFMON on Ubuntu (perf_event_paranoid is locked down).
//! Instead we use the world-readable RC6 residency counter: the GPU enters
//! RC6 as soon as its engines go idle, so `1 - ΔRC6 / Δt` is the fraction of
//! time the GPU was awake doing work. Under load this tracks render/media
//! busyness closely; at idle it reads ~0-3 %.

use super::{first_existing, GpuSample};
use crate::util::KFile;
use std::path::Path;

pub struct I915 {
    rc6: KFile,
    act_freq: Option<KFile>,
    max_freq: Option<u32>,
    last_rc6_ms: u64,
    last_us: i64,
}

impl I915 {
    pub fn new(card: &Path) -> Option<I915> {
        let rc6 = first_existing(&[card.join("gt/gt0/rc6_residency_ms"), card.join("power/rc6_residency_ms")])?;
        let act_freq = first_existing(&[card.join("gt/gt0/rps_act_freq_mhz"), card.join("gt_act_freq_mhz")]);
        let max_freq = first_existing(&[
            card.join("gt/gt0/rps_RP0_freq_mhz"),
            card.join("gt_RP0_freq_mhz"),
            card.join("gt_max_freq_mhz"),
        ])
        .and_then(|mut f| f.read_u64())
        .map(|v| v as u32);
        Some(I915 { rc6, act_freq, max_freq, last_rc6_ms: 0, last_us: 0 })
    }

    pub fn sample(&mut self, now_us: i64, out: &mut GpuSample) {
        out.freq_max_mhz = self.max_freq;
        out.freq_mhz = self.act_freq.as_mut().and_then(|f| f.read_u64()).map(|v| v as u32);
        let Some(rc6) = self.rc6.read_u64() else {
            out.busy = None;
            return;
        };
        out.awake = residency_busy(self.last_us, self.last_rc6_ms, now_us, rc6).or(out.awake);
        out.busy = out.awake;
        self.last_rc6_ms = rc6;
        self.last_us = now_us;
    }
}

/// Newer Intel GPUs (Meteor Lake, Lunar Lake, Arc) on the `xe` driver.
pub struct Xe {
    idle: KFile,
    act_freq: Option<KFile>,
    max_freq: Option<u32>,
    last_idle_ms: u64,
    last_us: i64,
}

impl Xe {
    pub fn new(dev: &Path) -> Option<Xe> {
        let gt = dev.join("tile0/gt0");
        let idle = first_existing(&[gt.join("gtidle/idle_residency_ms")])?;
        let act_freq = first_existing(&[gt.join("freq0/act_freq")]);
        let max_freq = first_existing(&[gt.join("freq0/rp0_freq"), gt.join("freq0/max_freq")])
            .and_then(|mut f| f.read_u64())
            .map(|v| v as u32);
        Some(Xe { idle, act_freq, max_freq, last_idle_ms: 0, last_us: 0 })
    }

    pub fn sample(&mut self, now_us: i64, out: &mut GpuSample) {
        out.freq_max_mhz = self.max_freq;
        out.freq_mhz = self.act_freq.as_mut().and_then(|f| f.read_u64()).map(|v| v as u32);
        let Some(idle) = self.idle.read_u64() else {
            out.busy = None;
            return;
        };
        out.awake = residency_busy(self.last_us, self.last_idle_ms, now_us, idle).or(out.awake);
        out.busy = out.awake;
        self.last_idle_ms = idle;
        self.last_us = now_us;
    }
}

/// Converts an idle-residency counter (ms) into a busy fraction over the
/// interval since the previous reading.
fn residency_busy(last_us: i64, last_ms: u64, now_us: i64, cur_ms: u64) -> Option<f32> {
    if last_us == 0 || now_us <= last_us || cur_ms < last_ms {
        return None;
    }
    let dt_ms = (now_us - last_us) as f64 / 1000.0;
    let idle = (cur_ms - last_ms) as f64 / dt_ms;
    Some((1.0 - idle).clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residency_math() {
        assert_eq!(residency_busy(0, 0, 100_000, 50), None);
        // 100 ms window, 75 ms idle → 25 % busy
        let b = residency_busy(1_000_000, 1000, 1_100_000, 1075).unwrap();
        assert!((b - 0.25).abs() < 1e-6);
        // counter granularity overshoot clamps to 0
        assert_eq!(residency_busy(1_000_000, 1000, 1_100_000, 1101), Some(0.0));
    }
}
