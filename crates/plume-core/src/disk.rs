//! Disk throughput / active time from `/proc/diskstats`, plus filesystem usage.

use crate::util::{fields, lines, parse_u64, read_string, read_u64_file, KFile};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Disk {
    pub name: String,
    pub model: String,
    pub size: u64,
    pub rotational: bool,
    /// Bytes per second.
    pub read_rate: f64,
    pub write_rate: f64,
    /// Fraction of wall time the device had I/O in flight (0..1).
    pub busy: f32,
    pub read_iops: f64,
    pub write_iops: f64,
    pub read_bytes: u64,
    pub written_bytes: u64,
    reads: u64,
    writes: u64,
    sectors_read: u64,
    sectors_written: u64,
    io_ms: u64,
    seen: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DiskSample {
    pub read_rate: f64,
    pub write_rate: f64,
    /// Busiest device's active time.
    pub busy: f32,
    pub read_iops: f64,
    pub write_iops: f64,
    /// Since boot.
    pub read_bytes: u64,
    pub written_bytes: u64,
    pub disks: Vec<Disk>,
}

pub struct DiskReader {
    file: KFile,
    sys_block: PathBuf,
    disks: Vec<Disk>,
    rejected: Vec<String>,
    last_us: i64,
}

impl DiskReader {
    pub fn new() -> DiskReader {
        Self::with_root(Path::new("/"))
    }
    pub fn with_root(root: &Path) -> DiskReader {
        DiskReader {
            file: KFile::open(root.join("proc/diskstats")),
            sys_block: root.join("sys/block"),
            disks: Vec::new(),
            rejected: Vec::new(),
            last_us: 0,
        }
    }

    pub fn sample(&mut self, now_us: i64, out: &mut DiskSample) -> bool {
        let Some(buf) = self.file.read() else { return false };
        let dt = if self.last_us > 0 { (now_us - self.last_us) as f64 / 1e6 } else { 0.0 };
        for d in &mut self.disks {
            d.seen = false;
        }
        for line in lines(buf) {
            let mut f = fields(line);
            let (Some(_maj), Some(_min), Some(name)) = (f.next(), f.next(), f.next()) else { continue };
            // reads, reads merged, sectors read, ms reading, writes, writes merged,
            // sectors written, ms writing, in flight, ms doing I/O
            let mut v = [0u64; 10];
            for slot in v.iter_mut() {
                *slot = f.next().map(parse_u64).unwrap_or(0);
            }
            let idx = match self.disks.iter().position(|d| d.name.as_bytes() == name) {
                Some(i) => i,
                None => {
                    let name = String::from_utf8_lossy(name).into_owned();
                    if self.rejected.contains(&name) {
                        continue;
                    }
                    match probe_disk(&self.sys_block, &name) {
                        Some(mut d) => {
                            d.sectors_read = v[2];
                            d.sectors_written = v[6];
                            d.io_ms = v[9];
                            d.reads = v[0];
                            d.writes = v[4];
                            self.disks.push(d);
                            self.disks.len() - 1
                        }
                        None => {
                            self.rejected.push(name);
                            continue;
                        }
                    }
                }
            };
            let d = &mut self.disks[idx];
            if dt > 0.0 {
                d.read_rate = v[2].saturating_sub(d.sectors_read) as f64 * 512.0 / dt;
                d.write_rate = v[6].saturating_sub(d.sectors_written) as f64 * 512.0 / dt;
                d.busy = (v[9].saturating_sub(d.io_ms) as f64 / (dt * 1000.0)).clamp(0.0, 1.0) as f32;
                d.read_iops = v[0].saturating_sub(d.reads) as f64 / dt;
                d.write_iops = v[4].saturating_sub(d.writes) as f64 / dt;
            }
            d.reads = v[0];
            d.writes = v[4];
            d.read_bytes = v[2] * 512;
            d.written_bytes = v[6] * 512;
            d.sectors_read = v[2];
            d.sectors_written = v[6];
            d.io_ms = v[9];
            d.seen = true;
        }
        self.disks.retain(|d| d.seen);
        self.last_us = now_us;
        out.read_rate = self.disks.iter().map(|d| d.read_rate).sum();
        out.write_rate = self.disks.iter().map(|d| d.write_rate).sum();
        out.busy = self.disks.iter().map(|d| d.busy).fold(0.0, f32::max);
        out.read_iops = self.disks.iter().map(|d| d.read_iops).sum();
        out.write_iops = self.disks.iter().map(|d| d.write_iops).sum();
        out.read_bytes = self.disks.iter().map(|d| d.read_bytes).sum();
        out.written_bytes = self.disks.iter().map(|d| d.written_bytes).sum();
        out.disks.clone_from(&self.disks);
        dt > 0.0
    }
}

impl Default for DiskReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Accepts whole physical disks only: present in /sys/block with a backing
/// device. This skips partitions, loop devices (snaps!), zram, dm-crypt and
/// LVM mappings that would otherwise double-count the same I/O.
fn probe_disk(sys_block: &Path, name: &str) -> Option<Disk> {
    if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram") {
        return None;
    }
    let dir = sys_block.join(name);
    if !dir.join("device").exists() {
        return None;
    }
    let model = read_string(dir.join("device/model"))
        .or_else(|| read_string(dir.join("device/name")))
        .unwrap_or_default();
    Some(Disk {
        name: name.to_string(),
        model,
        size: read_u64_file(dir.join("size")).unwrap_or(0) * 512,
        rotational: read_u64_file(dir.join("queue/rotational")) == Some(1),
        ..Default::default()
    })
}

/// A mounted filesystem backed by a block device.
#[derive(Clone, Debug, Default)]
pub struct Filesystem {
    pub device: String,
    pub mount: String,
    pub fstype: String,
    pub total: u64,
    pub used: u64,
    pub avail: u64,
}

/// Lists real filesystems (deduplicated by device) with usage. Cheap enough to
/// call every few seconds: one read of /proc/self/mounts plus one statvfs each.
pub fn filesystems() -> Vec<Filesystem> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mounts") else { return Vec::new() };
    let mut out: Vec<Filesystem> = Vec::new();
    for line in text.lines() {
        let mut f = line.split(' ');
        let (Some(dev), Some(mnt), Some(fstype)) = (f.next(), f.next(), f.next()) else { continue };
        if !dev.starts_with("/dev/") || dev.starts_with("/dev/loop") {
            continue;
        }
        if fstype == "squashfs" || out.iter().any(|o| o.device == dev) {
            continue;
        }
        let mnt = unescape_mount(mnt);
        let Ok(c) = std::ffi::CString::new(mnt.as_bytes()) else { continue };
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            continue;
        }
        let bs = st.f_frsize as u64;
        let total = st.f_blocks as u64 * bs;
        if total == 0 {
            continue;
        }
        let free = st.f_bfree as u64 * bs;
        out.push(Filesystem {
            device: dev.to_string(),
            mount: mnt,
            fstype: fstype.to_string(),
            total,
            used: total.saturating_sub(free),
            avail: st.f_bavail as u64 * bs,
        });
    }
    out
}

/// /proc/mounts escapes spaces etc. as octal (`\040`).
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn filters_and_rates() {
        let root = std::env::temp_dir().join(format!("plume-disk-{}", std::process::id()));
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::create_dir_all(root.join("sys/block/nvme0n1/device")).unwrap();
        fs::create_dir_all(root.join("sys/block/loop0")).unwrap();
        fs::create_dir_all(root.join("sys/block/dm-0")).unwrap();
        fs::write(root.join("sys/block/nvme0n1/device/model"), "Samsung SSD 970 EVO\n").unwrap();
        fs::write(root.join("sys/block/nvme0n1/size"), "1000000\n").unwrap();
        let write = |sr: u64, sw: u64, io: u64| {
            fs::write(
                root.join("proc/diskstats"),
                format!(
                    " 259 0 nvme0n1 10 0 {sr} 5 10 0 {sw} 5 0 {io} 10 0 0 0 0\n\
 259 1 nvme0n1p1 10 0 {sr} 5 10 0 {sw} 5 0 {io} 10 0 0 0 0\n\
   7 0 loop0 10 0 99999 5 0 0 0 0 0 9 9 0 0 0 0\n\
 253 0 dm-0 10 0 {sr} 5 10 0 {sw} 5 0 {io} 10 0 0 0 0\n"
                ),
            )
            .unwrap();
        };
        write(0, 0, 0);
        let mut r = DiskReader::with_root(&root);
        let mut s = DiskSample::default();
        assert!(!r.sample(1_000_000, &mut s));
        write(2048, 4096, 250);
        assert!(r.sample(1_500_000, &mut s));
        assert_eq!(s.disks.len(), 1, "only the whole physical disk is counted");
        assert_eq!(s.read_rate, 2048.0 * 512.0 * 2.0);
        assert_eq!(s.write_rate, 4096.0 * 512.0 * 2.0);
        assert!((s.busy - 0.5).abs() < 1e-6);
        assert_eq!(s.read_bytes, 2048 * 512);
        assert_eq!(s.disks[0].model, "Samsung SSD 970 EVO");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unescape() {
        assert_eq!(unescape_mount("/media/My\\040Disk"), "/media/My Disk");
    }

    #[test]
    fn live_filesystems() {
        // Container may have none; just make sure it doesn't panic.
        let _ = filesystems();
    }
}
