//! GPU engine time per client from DRM fdinfo (i915, xe, amdgpu, msm, ...).
//!
//! Every open DRM file description exposes cumulative engine usage in
//! `/proc/<pid>/fdinfo/<fd>` ("drm-engine-render: 123 ns", or for xe
//! "drm-cycles-rcs" / "drm-total-cycles-rcs"). Summing the deltas of all
//! clients gives real engine utilisation, as intel_gpu_top shows, without
//! root, at least for the user's own processes (the compositor included,
//! since gnome-shell runs as the user).
//!
//! Finding DRM fds means walking fd tables, which is the expensive part, so
//! it is done incrementally: new processes once, processes with GPU fds every
//! few seconds, everything else every 30 s. Per sample we only read the
//! fdinfo files of known GPU clients (typically 10–30 small files).

use crate::util::{now_us, parse_u64};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrmGpuUsage {
    /// Busiest engine class, fraction 0..1.
    pub busy: f32,
    /// (engine class, busy fraction).
    pub engines: Vec<(String, f32)>,
}

#[derive(Default)]
struct Counters {
    /// engine → (ns or cycles, total cycles for cycle-based drivers)
    engines: HashMap<String, (u64, u64)>,
    seen: bool,
}

#[derive(Default)]
struct PidState {
    next_scan_us: i64,
    fds: Vec<u32>,
}

pub struct DrmScanner {
    pids: HashMap<i32, PidState>,
    clients: HashMap<(String, u64), Counters>,
    capacity: HashMap<(String, String), u64>,
    last_list_us: i64,
    last_us: i64,
    me: u32,
    root: bool,
}

const LIST_EVERY_US: i64 = 2_000_000;
const RESCAN_GPU_US: i64 = 5_000_000;
const RESCAN_OTHER_US: i64 = 30_000_000;

impl Default for DrmScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl DrmScanner {
    pub fn new() -> DrmScanner {
        DrmScanner {
            pids: HashMap::new(),
            clients: HashMap::new(),
            capacity: HashMap::new(),
            last_list_us: 0,
            last_us: 0,
            me: unsafe { libc::getuid() },
            root: unsafe { libc::geteuid() } == 0,
        }
    }

    /// Returns usage per PCI device ("0000:00:02.0") and GPU share per pid
    /// (fraction of one engine of its busiest class).
    pub fn sample(&mut self) -> (HashMap<String, DrmGpuUsage>, HashMap<i32, f32>) {
        let t = now_us();
        if t - self.last_list_us >= LIST_EVERY_US {
            self.refresh_pids(t);
            self.last_list_us = t;
        }
        self.rescan_fds(t);
        // Elapsed wall time in ns, to compare with the ns engine counters.
        let dt_ns = if self.last_us > 0 { (t - self.last_us) as f64 * 1000.0 } else { 0.0 };
        self.last_us = t;

        for c in self.clients.values_mut() {
            c.seen = false;
        }
        // Read fdinfo of every known DRM fd.
        let mut per_client: Vec<((String, u64), i32, HashMap<String, (u64, u64)>)> = Vec::new();
        let mut seen_keys: std::collections::HashSet<(String, u64)> = Default::default();
        let mut dead: Vec<(i32, u32)> = Vec::new();
        for (&pid, st) in &self.pids {
            for &fd in &st.fds {
                let path = format!("/proc/{pid}/fdinfo/{fd}");
                let Ok(text) = std::fs::read(&path) else {
                    dead.push((pid, fd));
                    continue;
                };
                if let Some((pdev, id, engines, caps)) = parse_fdinfo(&text) {
                    for (eng, cap) in caps {
                        self.capacity.insert((pdev.clone(), eng), cap);
                    }
                    let key = (pdev, id);
                    // Several fds can share one client (dup); count it once.
                    if seen_keys.insert(key.clone()) {
                        per_client.push((key, pid, engines));
                    }
                }
            }
        }
        for (pid, fd) in dead {
            if let Some(st) = self.pids.get_mut(&pid) {
                st.fds.retain(|&f| f != fd);
            }
        }

        let mut by_dev: HashMap<String, HashMap<String, f64>> = HashMap::new();
        let mut by_pid: HashMap<i32, HashMap<String, f64>> = HashMap::new();
        for (key, pid, engines) in per_client {
            let prev = self.clients.entry(key.clone()).or_default();
            let fresh = prev.engines.is_empty();
            for (eng, &(v, total)) in &engines {
                let frac = match prev.engines.get(eng) {
                    Some(&(pv, ptot)) if !fresh && dt_ns > 0.0 => {
                        if total > 0 {
                            // Cycle-based (xe): busy cycles / elapsed GPU cycles.
                            let dtot = total.saturating_sub(ptot);
                            if dtot > 0 { v.saturating_sub(pv) as f64 / dtot as f64 } else { 0.0 }
                        } else {
                            // Time-based (i915, amdgpu): busy ns / elapsed ns.
                            v.saturating_sub(pv) as f64 / dt_ns
                        }
                    }
                    _ => 0.0,
                };
                *by_dev.entry(key.0.clone()).or_default().entry(eng.clone()).or_default() += frac;
                *by_pid.entry(pid).or_default().entry(eng.clone()).or_default() += frac;
            }
            prev.engines = engines;
            prev.seen = true;
        }
        self.clients.retain(|_, c| c.seen);

        let mut out = HashMap::new();
        for (dev, engines) in by_dev {
            let mut list: Vec<(String, f32)> = engines
                .into_iter()
                .map(|(e, f)| {
                    let cap = self.capacity.get(&(dev.clone(), e.clone())).copied().unwrap_or(1).max(1);
                    (e, (f / cap as f64).clamp(0.0, 1.0) as f32)
                })
                .collect();
            list.sort_by(|a, b| a.0.cmp(&b.0));
            let busy = list.iter().map(|(_, f)| *f).fold(0.0, f32::max);
            out.insert(dev, DrmGpuUsage { busy, engines: list });
        }
        let pids = by_pid
            .into_iter()
            .map(|(pid, e)| (pid, e.values().copied().fold(0.0, f64::max).clamp(0.0, 1.0) as f32))
            .collect();
        (out, pids)
    }

    fn refresh_pids(&mut self, t: i64) {
        let Ok(rd) = std::fs::read_dir("/proc") else { return };
        let mut alive = std::collections::HashSet::new();
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
            alive.insert(pid);
            if self.pids.contains_key(&pid) {
                continue;
            }
            // Only processes whose fds we may read.
            let Ok(md) = std::fs::metadata(e.path()) else { continue };
            use std::os::unix::fs::MetadataExt;
            if !self.root && md.uid() != self.me {
                continue;
            }
            self.pids.insert(pid, PidState { next_scan_us: t, fds: Vec::new() });
        }
        self.pids.retain(|pid, _| alive.contains(pid));
    }

    /// Walks fd tables of processes that are due, within a 2 ms budget per
    /// call (the rest are picked up on later calls).
    fn rescan_fds(&mut self, t: i64) {
        const BUDGET_US: i64 = 2_000;
        let t0 = now_us();
        for (&pid, st) in self.pids.iter_mut() {
            if st.next_scan_us > t {
                continue;
            }
            if now_us() - t0 > BUDGET_US {
                break;
            }
            let dir = format!("/proc/{pid}/fd");
            let mut fds = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let Ok(target) = std::fs::read_link(e.path()) else { continue };
                    if target.as_os_str().as_encoded_bytes().starts_with(b"/dev/dri/") {
                        if let Some(fd) = e.file_name().to_str().and_then(|s| s.parse().ok()) {
                            fds.push(fd);
                        }
                    }
                }
            }
            st.next_scan_us = t + if fds.is_empty() { RESCAN_OTHER_US } else { RESCAN_GPU_US };
            st.fds = fds;
        }
    }
}

type Parsed = (String, u64, HashMap<String, (u64, u64)>, Vec<(String, u64)>);

/// Parses one fdinfo file: (pdev, client id, engine → (value, total cycles), capacities).
fn parse_fdinfo(b: &[u8]) -> Option<Parsed> {
    let text = std::str::from_utf8(b).ok()?;
    let mut pdev = None;
    let mut id = None;
    let mut engines: HashMap<String, (u64, u64)> = HashMap::new();
    let mut caps = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        if k == "drm-pdev" {
            pdev = Some(v.to_string());
        } else if k == "drm-client-id" {
            id = v.parse().ok();
        } else if let Some(e) = k.strip_prefix("drm-engine-capacity-") {
            caps.push((e.to_string(), parse_u64(v.as_bytes())));
        } else if let Some(e) = k.strip_prefix("drm-engine-") {
            // "123456 ns"
            engines.entry(e.to_string()).or_default().0 = parse_u64(v.as_bytes());
        } else if let Some(e) = k.strip_prefix("drm-total-cycles-") {
            engines.entry(e.to_string()).or_default().1 = parse_u64(v.as_bytes());
        } else if let Some(e) = k.strip_prefix("drm-cycles-") {
            engines.entry(e.to_string()).or_default().0 = parse_u64(v.as_bytes());
        }
    }
    Some((pdev?, id?, engines, caps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_i915_and_xe() {
        let i915 = b"pos:\t0\nflags:\t02100002\ndrm-driver:\ti915\ndrm-client-id:\t29\ndrm-pdev:\t0000:00:02.0\n\
drm-engine-render:\t11380263218 ns\ndrm-engine-copy:\t0 ns\ndrm-engine-video:\t42 ns\ndrm-engine-capacity-video:\t2\n";
        let (pdev, id, eng, caps) = parse_fdinfo(i915).unwrap();
        assert_eq!(pdev, "0000:00:02.0");
        assert_eq!(id, 29);
        assert_eq!(eng["render"], (11380263218, 0));
        assert_eq!(eng["video"], (42, 0));
        assert_eq!(caps, vec![("video".to_string(), 2)]);

        let xe = b"drm-driver: xe\ndrm-client-id: 3\ndrm-pdev: 0000:00:02.0\ndrm-cycles-rcs: 28257900\ndrm-total-cycles-rcs: 7655183225\n";
        let (_, _, eng, _) = parse_fdinfo(xe).unwrap();
        assert_eq!(eng["rcs"], (28257900, 7655183225));
        assert!(parse_fdinfo(b"pos: 0\nflags: 0\n").is_none(), "not a DRM fd");
    }

    #[test]
    fn live_scan_does_not_panic() {
        let mut s = DrmScanner::new();
        let _ = s.sample();
        let _ = s.sample();
    }
}
