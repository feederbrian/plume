//! Process table scanner.
//!
//! Cost model (the dominant cost of any process manager): per process per scan
//! we do `openat+read+close` of `stat` and `statm`, plus `io` for processes we
//! own. `/proc` is enumerated with raw `getdents64` into a reused buffer, paths
//! are formatted into a stack buffer, and known processes are updated in place,
//! so a steady-state scan performs no heap allocation except the output Vec.
//! `cmdline`, owner and name are only read when a process first appears or
//! exec()s (detected by a change of `comm`).

use crate::util::{fields, now_us, page_size, parse_i64, parse_u64, read_at, clock_ticks};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

const PF_KTHREAD: u64 = 0x0020_0000;
const CMDLINE_MAX: usize = 4096;

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    pub uid: u32,
    /// Start time in clock ticks since boot; (pid, start) identifies a process.
    pub start_ticks: u64,
    pub name: Arc<str>,
    pub cmdline: Arc<str>,
    pub user: Arc<str>,
    pub state: u8,
    pub nice: i8,
    pub priority: i32,
    pub threads: u32,
    /// CPU usage as a fraction of one logical core (2.0 = two full cores).
    pub cpu: f32,
    /// Total CPU time consumed, in clock ticks.
    pub cpu_ticks: u64,
    pub rss: u64,
    pub shared: u64,
    /// Private resident memory (RSS minus file-backed/shared pages); the
    /// "Memory" column in GNOME System Monitor.
    pub memory: u64,
    pub vsize: u64,
    /// Storage I/O in bytes/s; `None` if the process isn't ours to inspect.
    pub read_rate: Option<f32>,
    pub write_rate: Option<f32>,
    pub read_total: u64,
    pub write_total: u64,
    pub kernel_thread: bool,
    /// Per-process TCP throughput; `None` when not measured or not ours to see.
    pub net: Option<crate::netproc::ProcNet>,
}

struct Entry {
    info: ProcInfo,
    comm: [u8; 64],
    comm_len: u8,
    seen: u32,
    io_denied: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScanStats {
    pub processes: usize,
    pub threads: usize,
    pub elapsed_us: i64,
}

pub struct ProcScanner {
    proc_fd: OwnedFd,
    dents: Vec<u8>,
    buf: Vec<u8>,
    entries: HashMap<i32, Entry>,
    users: HashMap<u32, Arc<str>>,
    page: u64,
    hz: f64,
    gen: u32,
    last_us: i64,
    /// Read `/proc/<pid>/io` (per-process disk rates). Costs one more open per
    /// owned process; turn off when the column is hidden.
    pub read_io: bool,
    empty: Arc<str>,
}

impl ProcScanner {
    pub fn new() -> std::io::Result<ProcScanner> {
        let fd = unsafe {
            libc::open(b"/proc\0".as_ptr() as *const libc::c_char, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(ProcScanner {
            proc_fd: unsafe { OwnedFd::from_raw_fd(fd) },
            dents: vec![0; 32 * 1024],
            buf: Vec::with_capacity(4096),
            entries: HashMap::with_capacity(512),
            users: HashMap::new(),
            page: page_size(),
            hz: clock_ticks() as f64,
            gen: 0,
            last_us: 0,
            read_io: true,
            empty: Arc::from(""),
        })
    }

    /// Scans all processes into `out` (cleared first).
    pub fn scan(&mut self, out: &mut Vec<ProcInfo>) -> ScanStats {
        let t0 = now_us();
        let dt = if self.last_us > 0 { (t0 - self.last_us) as f64 / 1e6 } else { 0.0 };
        self.gen = self.gen.wrapping_add(1);
        out.clear();
        let fd = self.proc_fd.as_raw_fd();
        unsafe { libc::lseek(fd, 0, libc::SEEK_SET) };
        let mut threads = 0usize;
        loop {
            let n = unsafe {
                libc::syscall(libc::SYS_getdents64, fd, self.dents.as_mut_ptr(), self.dents.len())
            };
            if n <= 0 {
                break;
            }
            let n = n as usize;
            let mut off = 0;
            while off < n {
                let reclen = u16::from_ne_bytes([self.dents[off + 16], self.dents[off + 17]]) as usize;
                let name = &self.dents[off + 19..off + reclen];
                let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())];
                off += reclen;
                if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
                    continue;
                }
                let pid = parse_u64(name) as i32;
                let mut path = [0u8; 32];
                path[..name.len()].copy_from_slice(name);
                if let Some(info) = scan_one(
                    fd,
                    pid,
                    &mut path,
                    name.len(),
                    &mut self.buf,
                    &mut self.entries,
                    &mut self.users,
                    &self.empty,
                    self.gen,
                    self.page,
                    self.hz,
                    dt,
                    self.read_io,
                ) {
                    threads += info.threads as usize;
                    out.push(info);
                }
            }
        }
        let gen = self.gen;
        self.entries.retain(|_, e| e.seen == gen);
        self.last_us = t0;
        ScanStats { processes: out.len(), threads, elapsed_us: now_us() - t0 }
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_one(
    proc_fd: libc::c_int,
    pid: i32,
    path: &mut [u8; 32],
    plen: usize,
    buf: &mut Vec<u8>,
    entries: &mut HashMap<i32, Entry>,
    users: &mut HashMap<u32, Arc<str>>,
    empty: &Arc<str>,
    gen: u32,
    page: u64,
    hz: f64,
    dt: f64,
    read_io: bool,
) -> Option<ProcInfo> {
    let rel = |path: &mut [u8; 32], file: &[u8]| -> usize {
        path[plen] = b'/';
        path[plen + 1..plen + 1 + file.len()].copy_from_slice(file);
        path[plen + 1 + file.len()] = 0;
        plen + 2 + file.len()
    };

    // ---- stat ----
    let l = rel(path, b"stat");
    let n = read_at(proc_fd, &path[..l], buf)?;
    let st = parse_stat(&buf[..n])?;

    let entry = entries.get_mut(&pid);
    let fresh = match &entry {
        Some(e) => e.info.start_ticks != st.start_ticks || &e.comm[..e.comm_len as usize] != st.comm(),
        None => true,
    };
    let mut is_new = false;
    let entry = if fresh {
        // New process, PID reuse, or exec(): (re)read identity.
        path[plen] = 0;
        let uid = owner(proc_fd, &path[..plen + 1]).unwrap_or(u32::MAX);
        let l = rel(path, b"cmdline");
        let cmdline = read_at(proc_fd, &path[..l], buf).map(|n| clean_cmdline(&buf[..n.min(CMDLINE_MAX)]));
        let cmdline = cmdline.unwrap_or_default();
        let kernel_thread = st.flags & PF_KTHREAD != 0;
        let name = display_name(st.comm(), &cmdline, kernel_thread);
        let user = users.entry(uid).or_insert_with(|| user_name(uid)).clone();
        let mut comm = [0u8; 64];
        let cl = st.comm_len;
        comm[..cl].copy_from_slice(st.comm());
        let prev = entries.remove(&pid);
        let carry = prev.filter(|p| p.info.start_ticks == st.start_ticks);
        is_new = carry.is_none();
        let info = ProcInfo {
            pid,
            ppid: st.ppid,
            uid,
            start_ticks: st.start_ticks,
            name: Arc::from(name),
            cmdline: if cmdline.is_empty() { empty.clone() } else { Arc::from(cmdline) },
            user,
            state: st.state,
            nice: st.nice as i8,
            priority: st.priority as i32,
            threads: st.threads as u32,
            cpu: 0.0,
            cpu_ticks: carry.as_ref().map_or(st.utime + st.stime, |c| c.info.cpu_ticks),
            rss: 0,
            shared: 0,
            memory: 0,
            vsize: st.vsize,
            read_rate: None,
            write_rate: None,
            read_total: carry.as_ref().map_or(0, |c| c.info.read_total),
            write_total: carry.as_ref().map_or(0, |c| c.info.write_total),
            kernel_thread,
            net: None,
        };
        entries.entry(pid).or_insert(Entry { info, comm, comm_len: cl as u8, seen: gen, io_denied: false })
    } else {
        entry?
    };

    let e = entry;
    let ticks = st.utime + st.stime;
    // Brand-new processes start at 0 % (no baseline yet); exec() keeps its baseline.
    e.info.cpu = if dt > 0.0 && !is_new {
        (ticks.saturating_sub(e.info.cpu_ticks) as f64 / hz / dt) as f32
    } else {
        0.0
    };
    e.info.cpu_ticks = ticks;
    e.info.ppid = st.ppid;
    e.info.state = st.state;
    e.info.nice = st.nice as i8;
    e.info.priority = st.priority as i32;
    e.info.threads = st.threads as u32;
    e.info.vsize = st.vsize;
    e.seen = gen;

    // ---- statm ----
    let l = rel(path, b"statm");
    if let Some(n) = read_at(proc_fd, &path[..l], buf) {
        let mut f = fields(&buf[..n]);
        let _size = f.next();
        let resident = f.next().map(parse_u64).unwrap_or(0) * page;
        let shared = f.next().map(parse_u64).unwrap_or(0) * page;
        e.info.rss = resident;
        e.info.shared = shared;
        e.info.memory = resident.saturating_sub(shared);
    } else {
        e.info.rss = st.rss_pages * page;
        e.info.memory = e.info.rss;
    }

    // ---- io (own processes only) ----
    if read_io && !e.io_denied && !e.info.kernel_thread {
        let l = rel(path, b"io");
        match read_at(proc_fd, &path[..l], buf) {
            Some(n) => {
                let (r, w) = parse_io(&buf[..n]);
                if dt > 0.0 && !is_new && e.info.read_rate.is_some() {
                    e.info.read_rate = Some((r.saturating_sub(e.info.read_total) as f64 / dt) as f32);
                    e.info.write_rate = Some((w.saturating_sub(e.info.write_total) as f64 / dt) as f32);
                } else {
                    e.info.read_rate = Some(0.0);
                    e.info.write_rate = Some(0.0);
                }
                e.info.read_total = r;
                e.info.write_total = w;
            }
            None => {
                let err = std::io::Error::last_os_error().raw_os_error();
                if matches!(err, Some(libc::EACCES) | Some(libc::EPERM)) {
                    e.io_denied = true;
                }
            }
        }
    }
    Some(e.info.clone())
}

pub(crate) struct Stat {
    comm_buf: [u8; 64],
    comm_len: usize,
    pub state: u8,
    pub ppid: i32,
    pub flags: u64,
    pub utime: u64,
    pub stime: u64,
    pub priority: i64,
    pub nice: i64,
    pub threads: u64,
    pub start_ticks: u64,
    pub vsize: u64,
    pub rss_pages: u64,
}

/// Parses `/proc/<pid>/stat`. `comm` may contain spaces and parentheses, so
/// fields are located relative to the *last* ')'.
impl Stat {
    pub fn comm(&self) -> &[u8] {
        &self.comm_buf[..self.comm_len]
    }
}

pub(crate) fn parse_stat(b: &[u8]) -> Option<Stat> {
    let open = b.iter().position(|&c| c == b'(')?;
    let close = b.iter().rposition(|&c| c == b')')?;
    if close <= open {
        return None;
    }
    let comm = &b[open + 1..close];
    let mut comm_buf = [0u8; 64];
    let comm_len = comm.len().min(64);
    comm_buf[..comm_len].copy_from_slice(&comm[..comm_len]);
    let mut f = fields(b.get(close + 2..)?);
    let mut v = [&b""[..]; 22];
    for slot in v.iter_mut() {
        *slot = f.next()?;
    }
    Some(Stat {
        comm_buf,
        comm_len,
        state: v[0].first().copied().unwrap_or(b'?'),
        ppid: parse_i64(v[1]) as i32,
        flags: parse_u64(v[6]),
        utime: parse_u64(v[11]),
        stime: parse_u64(v[12]),
        priority: parse_i64(v[15]),
        nice: parse_i64(v[16]),
        threads: parse_u64(v[17]),
        start_ticks: parse_u64(v[19]),
        vsize: parse_u64(v[20]),
        rss_pages: parse_u64(v[21]),
    })
}

fn parse_io(b: &[u8]) -> (u64, u64) {
    let (mut r, mut w) = (0, 0);
    for line in b.split(|&c| c == b'\n') {
        if let Some(v) = line.strip_prefix(b"read_bytes: ") {
            r = parse_u64(v);
        } else if let Some(v) = line.strip_prefix(b"write_bytes: ") {
            w = parse_u64(v);
        }
    }
    (r, w)
}

fn owner(proc_fd: libc::c_int, rel: &[u8]) -> Option<u32> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::fstatat(proc_fd, rel.as_ptr() as *const libc::c_char, &mut st, 0) };
    (r == 0).then_some(st.st_uid)
}

fn clean_cmdline(b: &[u8]) -> String {
    let b = match b.iter().rposition(|&c| c != 0) {
        Some(end) => &b[..=end],
        None => return String::new(),
    };
    let s: Vec<u8> = b.iter().map(|&c| if c == 0 || c == b'\n' { b' ' } else { c }).collect();
    String::from_utf8_lossy(&s).into_owned()
}

/// `comm` is truncated to 15 bytes; prefer argv[0]'s basename when it extends it.
fn display_name(comm: &[u8], cmdline: &str, kernel: bool) -> String {
    let comm_s = String::from_utf8_lossy(comm);
    if !kernel && comm.len() >= 15 {
        if let Some(arg0) = cmdline.split(' ').next() {
            let base = arg0.rsplit('/').next().unwrap_or(arg0);
            if base.len() > comm.len() && base.starts_with(&*comm_s) {
                return base.to_string();
            }
        }
    }
    comm_s.into_owned()
}

fn user_name(uid: u32) -> Arc<str> {
    if uid == u32::MAX {
        return Arc::from("?");
    }
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    let mut buf = vec![0 as libc::c_char; 2048];
    let r = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    if r == 0 && !out.is_null() && !pw.pw_name.is_null() {
        let name = unsafe { std::ffi::CStr::from_ptr(pw.pw_name) };
        return Arc::from(name.to_string_lossy().as_ref());
    }
    Arc::from(uid.to_string().as_str())
}

/// Counts processes by listing `/proc` (no per-process reads; ~50 µs).
pub fn count_processes() -> usize {
    let Ok(rd) = std::fs::read_dir("/proc") else { return 0 };
    rd.flatten()
        .filter(|e| e.file_name().as_encoded_bytes().iter().all(u8::is_ascii_digit))
        .count()
}

/// Boot time (seconds since the epoch), for converting `start_ticks`.
pub fn boot_time() -> u64 {
    let s = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix("btime ").map(|v| v.trim().parse().unwrap_or(0)))
        .unwrap_or(0)
}

/// Extra, comparatively expensive information for the process inspector.
#[derive(Clone, Debug, Default)]
pub struct ProcDetails {
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub cmdline: String,
    pub cgroup: Option<String>,
    pub unit: Option<String>,
    pub open_fds: Option<usize>,
    pub rss_anon: Option<u64>,
    pub rss_file: Option<u64>,
    pub rss_shmem: Option<u64>,
    pub swap: Option<u64>,
    pub vm_size: Option<u64>,
    pub vm_peak: Option<u64>,
    pub ctx_voluntary: Option<u64>,
    pub ctx_involuntary: Option<u64>,
    pub oom_score: Option<i64>,
    pub cpus_allowed: Option<String>,
    pub seccomp: Option<u64>,
}

pub fn inspect(pid: i32) -> Option<ProcDetails> {
    let base = format!("/proc/{pid}");
    let status = std::fs::read_to_string(format!("{base}/status")).ok()?;
    let mut d = ProcDetails {
        exe: std::fs::read_link(format!("{base}/exe")).ok().map(|p| p.to_string_lossy().into_owned()),
        cwd: std::fs::read_link(format!("{base}/cwd")).ok().map(|p| p.to_string_lossy().into_owned()),
        cmdline: std::fs::read(format!("{base}/cmdline")).map(|b| clean_cmdline(&b)).unwrap_or_default(),
        open_fds: std::fs::read_dir(format!("{base}/fd")).ok().map(|r| r.count()),
        oom_score: std::fs::read_to_string(format!("{base}/oom_score")).ok().and_then(|s| s.trim().parse().ok()),
        ..Default::default()
    };
    let kb = |v: &str| v.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()).map(|n| n * 1024);
    for line in status.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k {
            "RssAnon" => d.rss_anon = kb(v),
            "RssFile" => d.rss_file = kb(v),
            "RssShmem" => d.rss_shmem = kb(v),
            "VmSwap" => d.swap = kb(v),
            "VmSize" => d.vm_size = kb(v),
            "VmPeak" => d.vm_peak = kb(v),
            "voluntary_ctxt_switches" => d.ctx_voluntary = v.parse().ok(),
            "nonvoluntary_ctxt_switches" => d.ctx_involuntary = v.parse().ok(),
            "Cpus_allowed_list" => d.cpus_allowed = Some(v.to_string()),
            "Seccomp" => d.seccomp = v.parse().ok(),
            _ => {}
        }
    }
    if let Ok(cg) = std::fs::read_to_string(format!("{base}/cgroup")) {
        // cgroup v2: "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-firefox-1234.scope"
        let path = cg.lines().find_map(|l| l.strip_prefix("0::")).map(str::to_string);
        d.unit = path.as_deref().and_then(|p| {
            p.rsplit('/').find(|c| c.ends_with(".service") || c.ends_with(".scope")).map(str::to_string)
        });
        d.cgroup = path;
    }
    Some(d)
}

/// Thread IDs of a process (for renicing every thread, not just the leader).
pub fn thread_ids(pid: i32) -> Vec<i32> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .map(|r| r.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default()
}

/// Reads the start time of `pid` (to verify identity before acting on it).
pub fn start_ticks(pid: i32) -> Option<u64> {
    let b = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&b).map(|s| s.start_ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_hostile_comm() {
        let line = b"4242 (we) ird (name)) S 1 4242 4242 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 7 0 123456 99999999 2500 18446744073709551615";
        let s = parse_stat(line).unwrap();
        assert_eq!(s.comm(), b"we) ird (name)");
        assert_eq!(s.state, b'S');
        assert_eq!(s.ppid, 1);
        assert_eq!(s.utime, 250);
        assert_eq!(s.stime, 50);
        assert_eq!(s.priority, 20);
        assert_eq!(s.nice, 0);
        assert_eq!(s.threads, 7);
        assert_eq!(s.start_ticks, 123456);
        assert_eq!(s.vsize, 99999999);
        assert_eq!(s.rss_pages, 2500);
    }

    #[test]
    fn names() {
        assert_eq!(display_name(b"gnome-shell-cal", "/usr/libexec/gnome-shell-calendar-server", false), "gnome-shell-calendar-server");
        assert_eq!(display_name(b"bash", "/bin/bash -l", false), "bash");
        assert_eq!(clean_cmdline(b"/bin/sleep\x0010\x00"), "/bin/sleep 10");
    }

    #[test]
    fn live_scan_sees_ourselves() {
        let mut s = ProcScanner::new().unwrap();
        let mut out = Vec::new();
        let st = s.scan(&mut out);
        assert!(st.processes >= 1);
        let me = std::process::id() as i32;
        let p = out.iter().find(|p| p.pid == me).expect("own process");
        assert!(p.rss > 0);
        assert!(p.threads >= 1);
        assert_eq!(start_ticks(me), Some(p.start_ticks));
        // Burn some CPU and confirm the delta shows up.
        let t = std::time::Instant::now();
        let mut x = 0u64;
        while t.elapsed().as_millis() < 120 {
            x = x.wrapping_mul(31).wrapping_add(7);
        }
        std::hint::black_box(x);
        s.scan(&mut out);
        let p = out.iter().find(|p| p.pid == me).unwrap();
        assert!(p.cpu > 0.2, "cpu = {}", p.cpu);
        assert!(p.read_rate.is_some());
    }

    #[test]
    fn inspect_self() {
        let d = inspect(std::process::id() as i32).unwrap();
        assert!(d.exe.is_some());
        assert!(d.open_fds.unwrap_or(0) > 0);
    }
}
