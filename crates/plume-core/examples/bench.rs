//! Measures the per-sample cost of each collector.
//! `cargo run --release --example bench [-- N_FAKE_PROCS]`
use plume_core::{cpu::*, disk::*, mem::*, net::*, proc::*, util::now_us};
use std::process::{Child, Command};

fn time<F: FnMut()>(label: &str, iters: u32, mut f: F) {
    f(); // warm
    let t = now_us();
    for _ in 0..iters {
        f();
    }
    let per = (now_us() - t) as f64 / iters as f64;
    println!("{label:<28} {per:>8.1} µs/sample");
}

fn rusage_us() -> i64 {
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) };
    (r.ru_utime.tv_sec + r.ru_stime.tv_sec) * 1_000_000 + (r.ru_utime.tv_usec + r.ru_stime.tv_usec)
}

fn main() {
    let fake: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(400);
    let mut kids: Vec<Child> = (0..fake).filter_map(|_| Command::new("sleep").arg("60").spawn().ok()).collect();

    let mut cpu = CpuReader::new();
    let mut cs = CpuSample::default();
    time("cpu (/proc/stat)", 2000, || { cpu.sample(&mut cs); });
    let mut freq = Vec::new();
    time("cpu freq (all cores)", 2000, || cpu.frequencies(&mut freq));
    let mut mem = MemReader::new();
    time("mem (/proc/meminfo)", 2000, || { mem.sample(); });
    let mut net = NetReader::new();
    let mut ns = NetSample::default();
    time("net (/proc/net/dev)", 2000, || { net.sample(now_us(), &mut ns); });
    let mut disk = DiskReader::new();
    let mut ds = DiskSample::default();
    time("disk (/proc/diskstats)", 2000, || { disk.sample(now_us(), &mut ds); });

    let mut sc = ProcScanner::new().unwrap();
    let mut out = Vec::new();
    let st = sc.scan(&mut out);
    time(&format!("process scan ({} procs)", st.processes), 50, || { sc.scan(&mut out); });
    sc.read_io = false;
    time("process scan (no io)", 50, || { sc.scan(&mut out); });

    // End-to-end: run the real sampler for 10 s at defaults and measure our CPU.
    let s = plume_core::Sampler::start(Default::default());
    s.set_want_procs(true);
    let (c0, w0) = (rusage_us(), now_us());
    std::thread::sleep(std::time::Duration::from_secs(10));
    let (c1, w1) = (rusage_us(), now_us());
    println!(
        "sampler @10 Hz + procs @1 Hz: {:.3} % of one core",
        (c1 - c0) as f64 / (w1 - w0) as f64 * 100.0
    );
    let rss = std::fs::read_to_string("/proc/self/status").unwrap();
    let rss = rss.lines().find(|l| l.starts_with("VmRSS")).unwrap_or("");
    println!("{rss}");
    drop(s);
    for k in &mut kids {
        let _ = k.kill();
        let _ = k.wait();
    }
}
