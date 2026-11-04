//! CPU utilisation (total + per logical core), frequency and package temperature.

use crate::util::{fields, lines, parse_u64, read_string, KFile};
use std::path::Path;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    /// Guest time is already folded into user/nice by the kernel.
    pub fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq + self.steal
    }
    pub fn idle_all(&self) -> u64 {
        self.idle + self.iowait
    }
    pub fn kernel(&self) -> u64 {
        self.system + self.irq + self.softirq
    }
}

/// One utilisation sample. All values are fractions in `0.0..=1.0`.
#[derive(Clone, Default, Debug)]
pub struct CpuSample {
    pub total: f32,
    pub kernel: f32,
    pub cores: Vec<f32>,
}

#[derive(Clone, Default, Debug)]
pub struct CpuInfo {
    pub model: String,
    pub logical: usize,
    pub physical: usize,
}

pub struct CpuReader {
    stat: KFile,
    prev: Vec<CpuTimes>,
    cur: Vec<CpuTimes>,
    primed: bool,
    freq: Vec<KFile>,
    temp: Option<KFile>,
    pub info: CpuInfo,
}

impl CpuReader {
    pub fn new() -> CpuReader {
        Self::with_root(Path::new("/"))
    }

    pub fn with_root(root: &Path) -> CpuReader {
        let info = read_cpu_info(root);
        let n = info.logical.max(1);
        let freq = (0..n)
            .map(|i| KFile::open(root.join(format!("sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq"))))
            .collect();
        CpuReader {
            stat: KFile::open(root.join("proc/stat")),
            prev: vec![CpuTimes::default(); n + 1],
            cur: vec![CpuTimes::default(); n + 1],
            primed: false,
            freq,
            temp: find_package_temp(root).map(KFile::open),
            info,
        }
    }

    /// Takes a sample. The first call only primes the baseline and returns false.
    pub fn sample(&mut self, out: &mut CpuSample) -> bool {
        let Some(buf) = self.stat.read() else { return false };
        let n = parse_stat(buf, &mut self.cur);
        if n == 0 {
            return false;
        }
        if self.cur.len() > self.prev.len() {
            self.prev.resize(self.cur.len(), CpuTimes::default());
        }
        let ok = self.primed;
        if ok {
            let (t, k) = usage(&self.prev[0], &self.cur[0]);
            out.total = t;
            out.kernel = k;
            out.cores.resize(self.cur.len() - 1, 0.0);
            for i in 1..self.cur.len() {
                out.cores[i - 1] = usage(&self.prev[i], &self.cur[i]).0;
            }
        }
        std::mem::swap(&mut self.prev, &mut self.cur);
        self.primed = true;
        ok
    }

    /// Current frequency per logical core in MHz (0 where unavailable).
    pub fn frequencies(&mut self, out: &mut Vec<u32>) {
        out.clear();
        for f in &mut self.freq {
            out.push(f.read_u64().map(|khz| (khz / 1000) as u32).unwrap_or(0));
        }
    }

    /// Package temperature in °C.
    pub fn temperature(&mut self) -> Option<f32> {
        self.temp.as_mut()?.read_u64().map(|m| m as f32 / 1000.0)
    }
}

impl Default for CpuReader {
    fn default() -> Self {
        Self::new()
    }
}

fn usage(prev: &CpuTimes, cur: &CpuTimes) -> (f32, f32) {
    let dt = cur.total().saturating_sub(prev.total());
    if dt == 0 {
        return (0.0, 0.0);
    }
    let didle = cur.idle_all().saturating_sub(prev.idle_all());
    let dkern = cur.kernel().saturating_sub(prev.kernel());
    let busy = dt.saturating_sub(didle) as f32 / dt as f32;
    let kern = dkern as f32 / dt as f32;
    (busy.clamp(0.0, 1.0), kern.clamp(0.0, 1.0))
}

/// Parses `/proc/stat` cpu lines into `out` (index 0 = aggregate, i+1 = cpu i).
/// Returns the number of lines parsed. Offline CPUs keep their old slot.
pub fn parse_stat(buf: &[u8], out: &mut Vec<CpuTimes>) -> usize {
    let mut count = 0;
    for line in lines(buf) {
        if !line.starts_with(b"cpu") {
            // cpu lines come first; stop at the first non-cpu line.
            break;
        }
        let mut it = fields(line);
        let label = it.next().unwrap_or(b"");
        let idx = if label == b"cpu" { 0 } else { parse_u64(&label[3..]) as usize + 1 };
        let mut v = [0u64; 8];
        for slot in v.iter_mut() {
            match it.next() {
                Some(f) => *slot = parse_u64(f),
                None => break,
            }
        }
        if idx >= out.len() {
            out.resize(idx + 1, CpuTimes::default());
        }
        out[idx] = CpuTimes {
            user: v[0],
            nice: v[1],
            system: v[2],
            idle: v[3],
            iowait: v[4],
            irq: v[5],
            softirq: v[6],
            steal: v[7],
        };
        count += 1;
    }
    count
}

fn read_cpu_info(root: &Path) -> CpuInfo {
    let text = std::fs::read_to_string(root.join("proc/cpuinfo")).unwrap_or_default();
    let mut model = String::new();
    let mut logical = 0;
    let mut cores = std::collections::HashSet::new();
    let mut phys_id = String::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "processor" => logical += 1,
            "model name" if model.is_empty() => model = clean_model(v),
            "physical id" => phys_id = v.to_string(),
            "core id" => {
                cores.insert(format!("{phys_id}:{v}"));
            }
            _ => {}
        }
    }
    if logical == 0 {
        logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    }
    let physical = if cores.is_empty() { logical } else { cores.len() };
    CpuInfo { model, logical, physical }
}

/// "Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz" -> "Intel Core i7-9750H @ 2.60GHz"
pub fn clean_model(s: &str) -> String {
    let s = s.replace("(R)", "").replace("(TM)", "").replace(" CPU ", " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Finds the CPU package temperature sensor (coretemp / k10temp / zenpower).
fn find_package_temp(root: &Path) -> Option<std::path::PathBuf> {
    let hw = root.join("sys/class/hwmon");
    let mut fallback = None;
    for e in std::fs::read_dir(&hw).ok()?.flatten() {
        let dir = e.path();
        let name = read_string(dir.join("name")).unwrap_or_default();
        match name.as_str() {
            "coretemp" => {
                // temp1 is "Package id 0" on Intel.
                for i in 1..=4 {
                    let label = read_string(dir.join(format!("temp{i}_label"))).unwrap_or_default();
                    if label.starts_with("Package") {
                        return Some(dir.join(format!("temp{i}_input")));
                    }
                }
                return Some(dir.join("temp1_input"));
            }
            "k10temp" | "zenpower" => return Some(dir.join("temp1_input")),
            "cpu_thermal" | "acpitz" if fallback.is_none() => fallback = Some(dir.join("temp1_input")),
            _ => {}
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_A: &[u8] = b"cpu  100 0 50 800 50 0 0 0 0 0\n\
cpu0 50 0 25 400 25 0 0 0 0 0\n\
cpu1 50 0 25 400 25 0 0 0 0 0\n\
intr 12345\nctxt 999\n";
    const STAT_B: &[u8] = b"cpu  200 0 100 850 50 0 0 0 0 0\n\
cpu0 150 0 75 400 25 0 0 0 0 0\n\
cpu1 50 0 25 450 25 0 0 0 0 0\n\
intr 12345\n";

    #[test]
    fn parses_and_diffs() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        assert_eq!(parse_stat(STAT_A, &mut a), 3);
        assert_eq!(parse_stat(STAT_B, &mut b), 3);
        let (t, k) = usage(&a[0], &b[0]);
        // dt = 1150-1000 = 200 ... total: a=1000, b=1200 → dt=200, idle Δ=50 → busy 150/200
        assert!((t - 0.75).abs() < 1e-6, "{t}");
        assert!((k - 0.25).abs() < 1e-6, "{k}");
        assert!((usage(&a[1], &b[1]).0 - 1.0).abs() < 1e-6);
        assert!(usage(&a[2], &b[2]).0.abs() < 1e-6);
    }

    #[test]
    fn model_cleanup() {
        assert_eq!(clean_model("Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz"), "Intel Core i7-9750H @ 2.60GHz");
    }

    #[test]
    fn live_sample() {
        let mut r = CpuReader::new();
        let mut s = CpuSample::default();
        assert!(!r.sample(&mut s));
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(r.sample(&mut s));
        assert_eq!(s.cores.len(), r.info.logical);
        assert!((0.0..=1.0).contains(&s.total));
    }
}
