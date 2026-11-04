//! Per-process network throughput (the "Network" tab), without root.
//!
//! Linux has no per-process byte counters like macOS's. We approximate them:
//!
//! 1. One `NETLINK_SOCK_DIAG` dump per scan returns every TCP socket (IPv4 and
//!    IPv6) with its `tcp_info`, which carries `bytes_acked` (sent) and
//!    `bytes_received` (kernel ≥ 4.2). Unprivileged users may read this.
//! 2. Socket → process: the socket's inode appears as `socket:[N]` among the
//!    owner's `/proc/<pid>/fd` links. Resolved inodes are cached, and only
//!    *new* inodes trigger an fd walk, limited to processes of the socket's
//!    uid, busiest first, under a time budget.
//! 3. Per-process rate = Σ Δbytes of its sockets / Δt.
//!
//! Limits (shown in the UI): TCP only (UDP/QUIC traffic is not attributed),
//! and other users' processes can only be resolved when running as root.

use crate::proc::ProcInfo;
use crate::util::now_us;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const NETLINK_SOCK_DIAG: libc::c_int = 4;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const INET_DIAG_INFO: u16 = 2;
const TCPI_BYTES_ACKED: usize = 120;
const TCPI_BYTES_RECEIVED: usize = 128;
/// Max time spent walking fd tables per scan.
const RESOLVE_BUDGET_US: i64 = 4_000;

#[derive(Clone, Copy, Debug)]
struct Sock {
    inode: u64,
    uid: u32,
    sent: u64,
    recv: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcNet {
    pub rx_rate: f32,
    pub tx_rate: f32,
    /// Bytes on sockets seen since Plume started watching.
    pub rx_total: u64,
    pub tx_total: u64,
    pub connections: u32,
}

pub struct NetProcScanner {
    nl: Option<OwnedFd>,
    buf: Vec<u8>,
    socks: Vec<Sock>,
    prev: HashMap<u64, (u64, u64)>,
    owner: HashMap<u64, i32>,
    totals: HashMap<i32, (u64, u64)>,
    primed: bool,
    last_us: i64,
    root: bool,
}

impl Default for NetProcScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl NetProcScanner {
    pub fn new() -> NetProcScanner {
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, NETLINK_SOCK_DIAG) };
        NetProcScanner {
            nl: (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) }),
            buf: vec![0; 64 * 1024],
            socks: Vec::with_capacity(256),
            prev: HashMap::new(),
            owner: HashMap::new(),
            totals: HashMap::new(),
            primed: false,
            last_us: 0,
            root: unsafe { libc::geteuid() } == 0,
        }
    }

    pub fn available(&self) -> bool {
        self.nl.is_some()
    }

    /// Returns per-pid network figures. `procs` (from the process scanner) is
    /// used to pick candidate processes when resolving new sockets.
    pub fn sample(&mut self, procs: &[ProcInfo]) -> HashMap<i32, ProcNet> {
        let t = now_us();
        let dt = if self.last_us > 0 { (t - self.last_us) as f64 / 1e6 } else { 0.0 };
        self.last_us = t;
        self.socks.clear();
        let mut socks = std::mem::take(&mut self.socks);
        for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
            self.dump(family, &mut socks);
        }
        self.resolve(&socks, procs);

        let mut out: HashMap<i32, ProcNet> = HashMap::new();
        let mut next_prev = HashMap::with_capacity(socks.len());
        for s in &socks {
            if s.inode == 0 {
                continue;
            }
            let (ds, dr) = match self.prev.get(&s.inode) {
                Some(&(ps, pr)) => (s.sent.saturating_sub(ps), s.recv.saturating_sub(pr)),
                // A socket born since the last scan: everything it moved is new.
                None if self.primed => (s.sent, s.recv),
                None => (0, 0),
            };
            next_prev.insert(s.inode, (s.sent, s.recv));
            let Some(&pid) = self.owner.get(&s.inode) else { continue };
            let e = out.entry(pid).or_default();
            e.connections += 1;
            if dt > 0.0 {
                e.tx_rate += (ds as f64 / dt) as f32;
                e.rx_rate += (dr as f64 / dt) as f32;
            }
            let tot = self.totals.entry(pid).or_default();
            tot.0 += dr;
            tot.1 += ds;
        }
        for (pid, e) in out.iter_mut() {
            if let Some(&(r, s)) = self.totals.get(pid) {
                e.rx_total = r;
                e.tx_total = s;
            }
        }
        // Keep totals for processes that still exist even if they have no sockets now.
        let alive: std::collections::HashSet<i32> = procs.iter().map(|p| p.pid).collect();
        self.totals.retain(|pid, _| alive.contains(pid));
        for (pid, &(r, s)) in &self.totals {
            out.entry(*pid).or_insert(ProcNet { rx_total: r, tx_total: s, ..Default::default() });
        }
        self.owner.retain(|ino, _| next_prev.contains_key(ino));
        self.prev = next_prev;
        self.socks = socks;
        self.primed = true;
        out
    }

    fn dump(&mut self, family: u8, out: &mut Vec<Sock>) {
        let Some(nl) = self.nl.as_ref() else { return };
        let fd = nl.as_raw_fd();
        // nlmsghdr (16) + inet_diag_req_v2 (56)
        let mut req = [0u8; 72];
        req[0..4].copy_from_slice(&72u32.to_ne_bytes());
        req[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        let flags: u16 = (libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16;
        req[6..8].copy_from_slice(&flags.to_ne_bytes());
        req[8..12].copy_from_slice(&1u32.to_ne_bytes());
        req[16] = family;
        req[17] = libc::IPPROTO_TCP as u8;
        req[18] = 1 << (INET_DIAG_INFO - 1);
        req[20..24].copy_from_slice(&0xfffu32.to_ne_bytes()); // all TCP states
        let mut sa: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        sa.nl_family = libc::AF_NETLINK as u16;
        let r = unsafe {
            libc::sendto(
                fd,
                req.as_ptr() as *const libc::c_void,
                req.len(),
                0,
                &sa as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            )
        };
        if r < 0 {
            return;
        }
        loop {
            let n = unsafe { libc::recv(fd, self.buf.as_mut_ptr() as *mut libc::c_void, self.buf.len(), 0) };
            if n <= 0 {
                return;
            }
            if parse_dump(&self.buf[..n as usize], out) {
                return;
            }
        }
    }

    /// Maps unknown socket inodes to pids by walking fd tables.
    fn resolve(&mut self, socks: &[Sock], procs: &[ProcInfo]) {
        let mut unknown: std::collections::HashSet<u64> =
            socks.iter().filter(|s| s.inode != 0 && !self.owner.contains_key(&s.inode)).map(|s| s.inode).collect();
        if unknown.is_empty() {
            return;
        }
        let uids: std::collections::HashSet<u32> =
            socks.iter().filter(|s| unknown.contains(&s.inode)).map(|s| s.uid).collect();
        let me = unsafe { libc::getuid() };
        let mut cands: Vec<&ProcInfo> = procs
            .iter()
            .filter(|p| !p.kernel_thread && uids.contains(&p.uid) && (self.root || p.uid == me))
            .collect();
        // Busiest first: a process opening connections is usually running.
        cands.sort_unstable_by(|a, b| b.cpu.total_cmp(&a.cpu));
        let t0 = now_us();
        let mut link = [0u8; 64];
        for p in cands {
            if unknown.is_empty() || now_us() - t0 > RESOLVE_BUDGET_US {
                break;
            }
            let dir = format!("/proc/{}/fd", p.pid);
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let path = e.path();
                let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else { continue };
                let n = unsafe { libc::readlink(c.as_ptr(), link.as_mut_ptr() as *mut libc::c_char, link.len()) };
                if n <= 0 {
                    continue;
                }
                let l = &link[..n as usize];
                if let Some(rest) = l.strip_prefix(b"socket:[") {
                    let ino = crate::util::parse_u64(rest);
                    if unknown.remove(&ino) {
                        self.owner.insert(ino, p.pid);
                    }
                }
            }
        }
    }
}

/// Parses one recv() worth of netlink messages. Returns true at NLMSG_DONE.
fn parse_dump(d: &[u8], out: &mut Vec<Sock>) -> bool {
    let u16_at = |o: usize| u16::from_ne_bytes([d[o], d[o + 1]]);
    let u32_at = |o: usize| u32::from_ne_bytes(d[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_ne_bytes(d[o..o + 8].try_into().unwrap());
    let mut off = 0;
    while off + 16 <= d.len() {
        let len = u32_at(off) as usize;
        let ty = u16_at(off + 4);
        if len < 16 || off + len > d.len() {
            return true;
        }
        match ty {
            NLMSG_DONE | NLMSG_ERROR => return true,
            SOCK_DIAG_BY_FAMILY if len >= 16 + 72 => {
                let m = off + 16;
                let uid = u32_at(m + 64);
                let inode = u32_at(m + 68) as u64;
                let (mut sent, mut recv) = (0, 0);
                let mut a = m + 72;
                while a + 4 <= off + len {
                    let alen = u16_at(a) as usize;
                    let aty = u16_at(a + 2);
                    if alen < 4 || a + alen > off + len {
                        break;
                    }
                    if aty == INET_DIAG_INFO && alen - 4 >= TCPI_BYTES_RECEIVED + 8 {
                        sent = u64_at(a + 4 + TCPI_BYTES_ACKED);
                        recv = u64_at(a + 4 + TCPI_BYTES_RECEIVED);
                    }
                    a += (alen + 3) & !3;
                }
                out.push(Sock { inode, uid, sent, recv });
            }
            _ => {}
        }
        off += (len + 3) & !3;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn attributes_loopback_tcp_to_this_process() {
        let mut nps = NetProcScanner::new();
        if !nps.available() {
            return;
        }
        let mut scanner = crate::proc::ProcScanner::new().unwrap();
        let mut procs = Vec::new();
        scanner.scan(&mut procs);
        nps.sample(&procs); // prime

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 1 << 20];
            let mut got = 0;
            while got < 4 << 20 {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                got += n;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            got
        });
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        c.write_all(&vec![7u8; 4 << 20]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        scanner.scan(&mut procs);
        let m = nps.sample(&procs);
        let me = m.get(&(std::process::id() as i32)).copied().unwrap_or_default();
        assert!(me.connections >= 2, "{me:?}");
        assert!(me.tx_total >= 4 << 20, "sent {me:?}");
        assert!(me.rx_total >= 4 << 20, "received {me:?}");
        drop(c);
        assert_eq!(t.join().unwrap(), 4 << 20);
    }
}
