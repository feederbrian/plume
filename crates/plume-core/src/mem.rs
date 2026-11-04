//! Memory and swap from `/proc/meminfo`.

use crate::util::{fields, lines, parse_u64, KFile};
use std::path::Path;

/// All values in bytes.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct MemSample {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub used: u64,
    /// Page cache + reclaimable slab (what "buff/cache" shows in `free`).
    pub cache: u64,
    pub shmem: u64,
    pub dirty: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

impl MemSample {
    pub fn used_frac(&self) -> f32 {
        if self.total == 0 { 0.0 } else { self.used as f32 / self.total as f32 }
    }
    pub fn swap_frac(&self) -> f32 {
        if self.swap_total == 0 { 0.0 } else { self.swap_used as f32 / self.swap_total as f32 }
    }
}

pub struct MemReader {
    file: KFile,
}

impl MemReader {
    pub fn new() -> MemReader {
        Self::with_root(Path::new("/"))
    }
    pub fn with_root(root: &Path) -> MemReader {
        MemReader { file: KFile::open(root.join("proc/meminfo")) }
    }
    pub fn sample(&mut self) -> Option<MemSample> {
        Some(parse_meminfo(self.file.read()?))
    }
}

impl Default for MemReader {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse_meminfo(buf: &[u8]) -> MemSample {
    let (mut total, mut free, mut avail, mut buffers, mut cached, mut srecl) = (0, 0, None, 0, 0, 0);
    let (mut shmem, mut dirty, mut swap_total, mut swap_free) = (0, 0, 0, 0);
    for line in lines(buf) {
        let Some(colon) = line.iter().position(|&c| c == b':') else { continue };
        let key = &line[..colon];
        let kb = fields(&line[colon + 1..]).next().map(parse_u64).unwrap_or(0) * 1024;
        match key {
            b"MemTotal" => total = kb,
            b"MemFree" => free = kb,
            b"MemAvailable" => avail = Some(kb),
            b"Buffers" => buffers = kb,
            b"Cached" => cached = kb,
            b"SReclaimable" => srecl = kb,
            b"Shmem" => shmem = kb,
            b"Dirty" => dirty = kb,
            b"SwapTotal" => swap_total = kb,
            b"SwapFree" => swap_free = kb,
            _ => {}
        }
    }
    let cache = (buffers + cached + srecl).saturating_sub(shmem);
    let available = avail.unwrap_or(free + cache);
    MemSample {
        total,
        free,
        available,
        used: total.saturating_sub(available),
        cache,
        shmem,
        dirty,
        swap_total,
        swap_used: swap_total.saturating_sub(swap_free),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let s = parse_meminfo(
            b"MemTotal:       16000000 kB\nMemFree:         2000000 kB\nMemAvailable:    9000000 kB\n\
Buffers:          500000 kB\nCached:          6000000 kB\nSwapCached:            0 kB\n\
Shmem:            500000 kB\nSReclaimable:     400000 kB\nSwapTotal:       4000000 kB\nSwapFree:        3000000 kB\n",
        );
        assert_eq!(s.total, 16_000_000 * 1024);
        assert_eq!(s.used, 7_000_000 * 1024);
        assert_eq!(s.cache, 6_400_000 * 1024);
        assert_eq!(s.swap_used, 1_000_000 * 1024);
        assert!((s.used_frac() - 0.4375).abs() < 1e-6);
    }

    #[test]
    fn live() {
        let s = MemReader::new().sample().unwrap();
        assert!(s.total > 0 && s.used <= s.total);
    }
}
