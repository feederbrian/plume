//! Background sampling threads and the state they publish.
//!
//! * **fast** thread (default 10 Hz): CPU, memory, network, disk, Intel/AMD GPU.
//!   ~0.1 ms of work per tick.
//! * **slow** thread (4 Hz tick): process table (default every 1 s, and only
//!   while someone is looking at it), NVIDIA via NVML, filesystems.
//!
//! Sampling is deliberately decoupled from drawing: the kernel only accounts
//! CPU time in 10 ms ticks, so sampling at 60 Hz would add noise, not detail.
//! The UI interpolates between samples and scrolls at the display's frame rate.
//!
//! Both threads raise their timer slack so the kernel can batch their wakeups
//! with other timers; samples carry exact timestamps, so the jitter is harmless.

use crate::cpu::{CpuInfo, CpuReader, CpuSample};
use crate::disk::{filesystems, DiskReader, DiskSample, Filesystem};
use crate::gpu::{self, Gpu, GpuInfo, GpuSample};
use crate::history::Series;
use crate::mem::{MemReader, MemSample};
use crate::net::{NetReader, NetSample};
use crate::proc::{ProcInfo, ProcScanner, ScanStats};
use crate::util::{now_us, KFile};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

#[derive(Default)]
pub struct History {
    pub cpu: Series,
    pub cpu_kernel: Series,
    pub cores: Vec<Series>,
    /// Fractions 0..1.
    pub mem: Series,
    pub swap: Series,
    /// Bytes per second.
    pub net_rx: Series,
    pub net_tx: Series,
    pub disk_read: Series,
    pub disk_write: Series,
    pub disk_busy: Series,
    pub gpu_busy: Vec<Series>,
    pub gpu_mem: Vec<Series>,
}

#[derive(Clone, Default)]
pub struct StaticInfo {
    pub cpu: CpuInfo,
    pub gpus: Vec<GpuInfo>,
    pub hostname: String,
    pub kernel: String,
    pub boot_time: u64,
}

/// Everything the UI reads. Guarded by one mutex; writers hold it for a few
/// microseconds (the process scan is built outside the lock and swapped in).
#[derive(Default)]
pub struct Shared {
    pub info: StaticInfo,
    pub hist: History,
    pub cpu: CpuSample,
    pub cpu_freq: Vec<u32>,
    pub cpu_temp: Option<f32>,
    pub mem: MemSample,
    pub net: NetSample,
    pub disk: DiskSample,
    pub gpus: Vec<GpuSample>,
    pub load: [f32; 3],
    pub uptime: f64,
    /// Totals that are cheap to get without a full process scan (1 Hz).
    pub n_procs: usize,
    pub n_threads: usize,
    /// Pressure stall info, "some" avg10 in percent: [cpu, memory, io].
    pub psi: [f32; 3],
    /// Whether per-process network figures are being collected.
    pub net_procs: bool,
    /// GPU engine share per pid (DRM fdinfo), refreshed by the fast thread.
    pub gpu_by_pid: std::collections::HashMap<i32, f32>,
    /// True once DRM fdinfo engine accounting has produced data.
    pub gpu_procs: bool,
    pub procs: Arc<Vec<ProcInfo>>,
    pub procs_gen: u64,
    pub proc_stats: ScanStats,
    pub filesystems: Vec<Filesystem>,
    /// Incremented after each fast-thread sample.
    pub fast_gen: u64,
    /// Cost of the last fast sample, µs (shown in the About/debug view).
    pub fast_cost_us: i64,
}

struct Control {
    stop: AtomicBool,
    want_procs: AtomicBool,
    want_io: AtomicBool,
    want_net: AtomicBool,
    idle: AtomicBool,
    scan_now: AtomicBool,
    fast_interval_us: AtomicU64,
    proc_interval_us: AtomicU64,
    wake: (Mutex<()>, Condvar),
}

pub struct Sampler {
    shared: Arc<Mutex<Shared>>,
    ctl: Arc<Control>,
    threads: Vec<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SamplerConfig {
    pub fast_hz: f64,
    pub proc_interval_ms: u64,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        SamplerConfig { fast_hz: 10.0, proc_interval_ms: 1000 }
    }
}

impl Sampler {
    pub fn start(cfg: SamplerConfig) -> Sampler {
        let root = Path::new("/");
        let cpu = CpuReader::with_root(root);
        let gpus = gpu::discover(root);
        let n_cores = cpu.info.logical;
        let n_gpus = gpus.len();

        let info = StaticInfo {
            cpu: cpu.info.clone(),
            gpus: gpus.iter().map(|g| g.info.clone()).collect(),
            hostname: crate::util::read_string("/proc/sys/kernel/hostname").unwrap_or_default(),
            kernel: crate::util::read_string("/proc/sys/kernel/osrelease").unwrap_or_default(),
            boot_time: crate::proc::boot_time(),
        };
        let mut shared = Shared { info, ..Default::default() };
        shared.hist.cores = (0..n_cores).map(|_| Series::new()).collect();
        shared.hist.gpu_busy = (0..n_gpus).map(|_| Series::new()).collect();
        shared.hist.gpu_mem = (0..n_gpus).map(|_| Series::new()).collect();
        shared.gpus = vec![GpuSample::default(); n_gpus];
        let shared = Arc::new(Mutex::new(shared));

        let ctl = Arc::new(Control {
            stop: AtomicBool::new(false),
            want_procs: AtomicBool::new(false),
            want_io: AtomicBool::new(true),
            want_net: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            scan_now: AtomicBool::new(false),
            fast_interval_us: AtomicU64::new(hz_to_us(cfg.fast_hz)),
            proc_interval_us: AtomicU64::new(cfg.proc_interval_ms.max(100) * 1000),
            wake: (Mutex::new(()), Condvar::new()),
        });

        // Partition GPUs by worker, remembering their global index.
        let (mut fast_gpus, mut slow_gpus) = (Vec::new(), Vec::new());
        for (i, g) in gpus.into_iter().enumerate() {
            if g.is_slow() { slow_gpus.push((i, g)) } else { fast_gpus.push((i, g)) }
        }

        let mut threads = Vec::new();
        {
            let (shared, ctl) = (shared.clone(), ctl.clone());
            threads.push(
                std::thread::Builder::new()
                    .name("plume-fast".into())
                    .spawn(move || fast_loop(shared, ctl, cpu, fast_gpus))
                    .expect("spawn sampler"),
            );
        }
        {
            let (shared, ctl) = (shared.clone(), ctl.clone());
            threads.push(
                std::thread::Builder::new()
                    .name("plume-slow".into())
                    .spawn(move || slow_loop(shared, ctl, slow_gpus))
                    .expect("spawn sampler"),
            );
        }
        Sampler { shared, ctl, threads }
    }

    pub fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn shared(&self) -> Arc<Mutex<Shared>> {
        self.shared.clone()
    }

    pub fn set_fast_hz(&self, hz: f64) {
        self.ctl.fast_interval_us.store(hz_to_us(hz), Ordering::Relaxed);
    }

    pub fn fast_interval_us(&self) -> u64 {
        self.ctl.fast_interval_us.load(Ordering::Relaxed)
    }

    pub fn set_proc_interval_ms(&self, ms: u64) {
        self.ctl.proc_interval_us.store(ms.max(100) * 1000, Ordering::Relaxed);
    }

    /// Process scanning only runs while the process list is on screen.
    pub fn set_want_procs(&self, on: bool) {
        let was = self.ctl.want_procs.swap(on, Ordering::Relaxed);
        if on && !was {
            self.scan_now();
        }
    }

    pub fn set_want_io(&self, on: bool) {
        self.ctl.want_io.store(on, Ordering::Relaxed);
    }

    /// Per-process network attribution (socket dump + fd walk) for the Network tab.
    pub fn set_want_net(&self, on: bool) {
        let was = self.ctl.want_net.swap(on, Ordering::Relaxed);
        if on && !was {
            self.scan_now();
        }
    }

    /// When nothing is visible, drop the fast tier to 1 Hz.
    pub fn set_idle(&self, idle: bool) {
        self.ctl.idle.store(idle, Ordering::Relaxed);
    }

    pub fn scan_now(&self) {
        self.ctl.scan_now.store(true, Ordering::Relaxed);
        self.ctl.wake.1.notify_all();
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.ctl.stop.store(true, Ordering::Relaxed);
        self.ctl.wake.1.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn hz_to_us(hz: f64) -> u64 {
    (1e6 / hz.clamp(0.5, 60.0)) as u64
}

fn set_timer_slack(ns: u64) {
    unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, ns as libc::c_ulong, 0, 0, 0) };
}

/// Sleeps until an absolute CLOCK_MONOTONIC deadline (µs), or until woken.
fn sleep_until(ctl: &Control, deadline_us: i64) {
    let now = now_us();
    if deadline_us <= now {
        return;
    }
    let guard = ctl.wake.0.lock().unwrap_or_else(|e| e.into_inner());
    let _ = ctl
        .wake
        .1
        .wait_timeout(guard, std::time::Duration::from_micros((deadline_us - now) as u64));
}

fn fast_loop(shared: Arc<Mutex<Shared>>, ctl: Arc<Control>, mut cpu: CpuReader, mut gpus: Vec<(usize, Gpu)>) {
    set_timer_slack(1_000_000);
    let mut mem = MemReader::new();
    let mut net = NetReader::new();
    let mut disk = DiskReader::new();
    let mut loadavg = KFile::open("/proc/loadavg");
    let mut psi = [
        KFile::open("/proc/pressure/cpu"),
        KFile::open("/proc/pressure/memory"),
        KFile::open("/proc/pressure/io"),
    ];
    let mut uptime = KFile::open("/proc/uptime");
    let mut cs = CpuSample::default();
    let mut ns = NetSample::default();
    let mut ds = DiskSample::default();
    let mut gs: Vec<GpuSample> = vec![GpuSample::default(); gpus.len()];
    // Real engine utilisation for DRM-driver GPUs (i915, xe, amdgpu).
    let mut drm = gpus
        .iter()
        .any(|(_, g)| matches!(g.info.driver.as_str(), "i915" | "xe" | "amdgpu"))
        .then(crate::gpu::drm::DrmScanner::new);
    let mut freq = Vec::new();
    let mut tick: u64 = 0;
    let mut next = now_us();
    while !ctl.stop.load(Ordering::Relaxed) {
        let t = now_us();
        let have_cpu = cpu.sample(&mut cs);
        let m = mem.sample();
        let have_net = net.sample(t, &mut ns);
        let have_disk = disk.sample(t, &mut ds);
        for ((_, g), s) in gpus.iter_mut().zip(gs.iter_mut()) {
            g.sample(t, s);
        }
        let mut gpu_pids = None;
        if let Some(d) = drm.as_mut() {
            let (devs, pids) = d.sample();
            for ((_, g), s) in gpus.iter().zip(gs.iter_mut()) {
                match devs.get(&g.info.pci_addr) {
                    Some(u) if s.power != crate::gpu::PowerState::Suspended => {
                        s.busy = Some(u.busy);
                        s.engines.clone_from(&u.engines);
                    }
                    _ => s.engines.clear(),
                }
            }
            gpu_pids = Some(pids);
        }
        // Slow-changing values at ~2 Hz / ~1 Hz.
        let interval = ctl.fast_interval_us.load(Ordering::Relaxed).max(1);
        let per_sec = (1_000_000 / interval).max(1);
        let do_freq = tick % (per_sec / 2).max(1) == 0;
        let do_misc = tick % per_sec == 0;
        let temp = if do_freq {
            cpu.frequencies(&mut freq);
            cpu.temperature()
        } else {
            None
        };
        let (load, up, counts) = if do_misc {
            let la = loadavg.read().map(|b| (parse_loadavg(b), parse_threads(b)));
            (
                la.map(|l| l.0),
                uptime.read().map(|b| crate::util::parse_u64(b) as f64),
                Some((
                    crate::proc::count_processes(),
                    la.map(|l| l.1).unwrap_or(0),
                    [psi[0].read().map(parse_psi), psi[1].read().map(parse_psi), psi[2].read().map(parse_psi)],
                )),
            )
        } else {
            (None, None, None)
        };
        let cost = now_us() - t;
        {
            let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
            let s = &mut *guard;
            let h = &mut s.hist;
            if have_cpu {
                h.cpu.push(t, cs.total);
                h.cpu_kernel.push(t, cs.kernel);
                if h.cores.len() < cs.cores.len() {
                    h.cores.resize_with(cs.cores.len(), Series::new);
                }
                for (series, v) in h.cores.iter_mut().zip(cs.cores.iter()) {
                    series.push(t, *v);
                }
            }
            if let Some(m) = m {
                h.mem.push(t, m.used_frac());
                h.swap.push(t, m.swap_frac());
            }
            if have_net {
                h.net_rx.push(t, ns.rx_rate as f32);
                h.net_tx.push(t, ns.tx_rate as f32);
            }
            if have_disk {
                h.disk_read.push(t, ds.read_rate as f32);
                h.disk_write.push(t, ds.write_rate as f32);
                h.disk_busy.push(t, ds.busy);
            }
            for ((i, _), g) in gpus.iter().zip(gs.iter()) {
                if let Some(b) = g.busy {
                    h.gpu_busy[*i].push(t, b);
                }
                if let (Some(u), Some(tot)) = (g.mem_used, g.mem_total) {
                    if tot > 0 {
                        h.gpu_mem[*i].push(t, u as f32 / tot as f32);
                    }
                }
                s.gpus[*i].clone_from(g);
            }
            if let Some(p) = gpu_pids.take() {
                s.gpu_procs = true;
                s.gpu_by_pid = p;
            }
            if have_cpu {
                s.cpu.clone_from(&cs);
            }
            if let Some(m) = m {
                s.mem = m;
            }
            if have_net {
                std::mem::swap(&mut s.net, &mut ns);
            }
            if have_disk {
                std::mem::swap(&mut s.disk, &mut ds);
            }
            if do_freq {
                s.cpu_freq.clone_from(&freq);
                s.cpu_temp = temp.or(s.cpu_temp);
            }
            if let Some(l) = load {
                s.load = l;
            }
            if let Some(u) = up {
                s.uptime = u;
            }
            if let Some((p, t, ps)) = counts {
                s.n_procs = p;
                s.n_threads = t;
                for (o, v) in s.psi.iter_mut().zip(ps) {
                    *o = v.unwrap_or(0.0);
                }
            }
            s.fast_cost_us = cost;
            s.fast_gen += 1;
        }
        tick += 1;
        let interval = if ctl.idle.load(Ordering::Relaxed) { 1_000_000 } else { interval as i64 };
        next += interval;
        let now = now_us();
        if next < now {
            next = now + interval; // fell behind (suspend/resume): don't burst
        }
        sleep_until(&ctl, next);
    }
}

fn slow_loop(shared: Arc<Mutex<Shared>>, ctl: Arc<Control>, mut gpus: Vec<(usize, Gpu)>) {
    set_timer_slack(5_000_000);
    const TICK_US: i64 = 250_000;
    let mut scanner = ProcScanner::new().ok();
    let mut netp = crate::netproc::NetProcScanner::new();
    let me = unsafe { libc::getuid() };
    let root = unsafe { libc::geteuid() } == 0;
    let mut procs: Vec<ProcInfo> = Vec::with_capacity(512);
    let mut gs: Vec<GpuSample> = vec![GpuSample::default(); gpus.len()];
    let mut last_scan: i64 = 0;
    let mut last_fs: i64 = 0;
    let mut next = now_us();
    while !ctl.stop.load(Ordering::Relaxed) {
        let t = now_us();
        for ((_, g), s) in gpus.iter_mut().zip(gs.iter_mut()) {
            g.sample(t, s);
        }
        let interval = ctl.proc_interval_us.load(Ordering::Relaxed) as i64;
        let scan_now = ctl.scan_now.swap(false, Ordering::Relaxed);
        let want = ctl.want_procs.load(Ordering::Relaxed) && !ctl.idle.load(Ordering::Relaxed);
        let mut stats = None;
        if let Some(sc) = scanner.as_mut() {
            if scan_now || (want && t - last_scan >= interval - TICK_US / 2) {
                sc.read_io = ctl.want_io.load(Ordering::Relaxed);
                stats = Some(sc.scan(&mut procs));
                last_scan = t;
                if ctl.want_net.load(Ordering::Relaxed) && netp.available() {
                    let m = netp.sample(&procs);
                    for p in procs.iter_mut() {
                        p.net = match m.get(&p.pid) {
                            Some(n) => Some(*n),
                            None if root || p.uid == me => Some(Default::default()),
                            None => None,
                        };
                    }
                }
            }
        }
        let fs = if t - last_fs >= 5_000_000 {
            last_fs = t;
            Some(filesystems())
        } else {
            None
        };
        {
            let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
            let s = &mut *guard;
            if stats.is_some() && s.gpu_procs {
                for p in procs.iter_mut() {
                    p.gpu = Some(s.gpu_by_pid.get(&p.pid).copied().unwrap_or(0.0));
                }
            }
            for ((i, _), g) in gpus.iter().zip(gs.iter()) {
                if let Some(b) = g.busy {
                    s.hist.gpu_busy[*i].push(t, b);
                }
                if let (Some(u), Some(tot)) = (g.mem_used, g.mem_total) {
                    if tot > 0 {
                        s.hist.gpu_mem[*i].push(t, u as f32 / tot as f32);
                    }
                }
                s.gpus[*i].clone_from(g);
            }
            if let Some(st) = stats {
                // Hand the Vec to the UI; reuse the previous one if it's free.
                let fresh = std::mem::take(&mut procs);
                let old = std::mem::replace(&mut s.procs, Arc::new(fresh));
                procs = Arc::try_unwrap(old).unwrap_or_else(|_| Vec::with_capacity(512));
                s.proc_stats = st;
                s.procs_gen += 1;
                s.net_procs = ctl.want_net.load(Ordering::Relaxed);
            }
            if let Some(fs) = fs {
                s.filesystems = fs;
            }
        }
        next += TICK_US;
        let now = now_us();
        if next < now {
            next = now + TICK_US;
        }
        sleep_until(&ctl, next);
    }
}

/// "some avg10=1.23 ..." → 1.23
fn parse_psi(b: &[u8]) -> f32 {
    let line = b.split(|&c| c == b'\n').next().unwrap_or(b"");
    crate::util::fields(line)
        .find_map(|f| f.strip_prefix(b"avg10="))
        .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
        .unwrap_or(0.0)
}

/// Total threads from the 4th field of /proc/loadavg ("running/total").
fn parse_threads(b: &[u8]) -> usize {
    crate::util::fields(b)
        .nth(3)
        .and_then(|f| f.split(|&c| c == b'/').nth(1))
        .map(|t| crate::util::parse_u64(t) as usize)
        .unwrap_or(0)
}

fn parse_loadavg(b: &[u8]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (o, f) in out.iter_mut().zip(crate::util::fields(b)) {
        *o = std::str::from_utf8(f).ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_publishes() {
        let s = Sampler::start(SamplerConfig { fast_hz: 50.0, proc_interval_ms: 100 });
        s.set_want_procs(true);
        std::thread::sleep(std::time::Duration::from_millis(400));
        {
            let g = s.lock();
            assert!(g.fast_gen >= 5, "fast_gen = {}", g.fast_gen);
            assert!(g.hist.cpu.len() >= 4);
            assert!(g.hist.mem.len() >= 4);
            assert!(!g.procs.is_empty());
            assert!(g.mem.total > 0);
            assert_eq!(g.hist.cores.len(), g.info.cpu.logical);
        }
        drop(s); // joins threads
    }

    #[test]
    fn loadavg() {
        assert_eq!(parse_loadavg(b"0.52 0.58 0.59 1/389 12345\n"), [0.52, 0.58, 0.59]);
        assert_eq!(parse_threads(b"0.52 0.58 0.59 1/389 12345\n"), 389);
        assert_eq!(parse_psi(b"some avg10=1.25 avg60=0.50 avg300=0.10 total=99\nfull avg10=0.00\n"), 1.25);
    }
}
