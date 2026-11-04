//! Low-level helpers: re-readable kernel files and allocation-free parsing.
//!
//! Every system-wide file (`/proc/stat`, `/proc/meminfo`, sysfs attributes, ...)
//! is opened once and re-read with `pread(fd, buf, len, 0)`. procfs and sysfs
//! regenerate their contents on a read at offset 0, so this avoids an
//! open/close pair per sample and keeps the steady state allocation-free.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

/// A kernel-generated file that is kept open and re-read from offset 0.
pub struct KFile {
    fd: Option<OwnedFd>,
    path: CString,
    buf: Vec<u8>,
}

impl KFile {
    /// Opens `path`. A missing or unreadable file yields a `KFile` whose
    /// `read` always returns `None`; callers treat that as "metric unavailable".
    pub fn open(path: impl AsRef<Path>) -> KFile {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_ref().as_os_str().as_bytes()).unwrap_or_default();
        let fd = open_cstr(&path);
        KFile { fd, path, buf: Vec::with_capacity(4096) }
    }

    pub fn exists(&self) -> bool {
        self.fd.is_some()
    }

    /// Reads the whole file. The returned slice borrows the internal buffer,
    /// which grows to fit the largest content seen and is then reused.
    pub fn read(&mut self) -> Option<&[u8]> {
        let fd = self.fd.as_ref()?.as_raw_fd();
        let n = match pread_all(fd, &mut self.buf) {
            Some(n) => n,
            None => {
                // Some files refuse pread; fall back to a fresh open.
                self.fd = open_cstr(&self.path);
                let fd = self.fd.as_ref()?.as_raw_fd();
                pread_all(fd, &mut self.buf)?
            }
        };
        Some(&self.buf[..n])
    }

    /// Reads and parses the file as a single unsigned integer.
    pub fn read_u64(&mut self) -> Option<u64> {
        let b = self.read()?;
        parse_u64_strict(trim(b))
    }

    /// Reads the file as trimmed text and compares it with `s`.
    pub fn read_eq(&mut self, s: &str) -> Option<bool> {
        let b = self.read()?;
        Some(trim(b) == s.as_bytes())
    }
}

fn open_cstr(path: &CString) -> Option<OwnedFd> {
    // SAFETY: path is NUL-terminated; the fd is owned by OwnedFd on success.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        None
    } else {
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

/// Reads an entire file from offset 0 into `buf` (growing it if needed).
/// Returns the number of bytes read.
pub(crate) fn pread_all(fd: libc::c_int, buf: &mut Vec<u8>) -> Option<usize> {
    if buf.capacity() < 4096 {
        buf.reserve(4096);
    }
    // SAFETY: we only expose bytes the kernel wrote (0..total).
    unsafe { buf.set_len(buf.capacity()) };
    let mut total = 0usize;
    loop {
        if total == buf.len() {
            let new_len = buf.len() * 2;
            buf.resize(new_len, 0);
        }
        let n = unsafe {
            libc::pread(
                fd,
                buf.as_mut_ptr().add(total) as *mut libc::c_void,
                buf.len() - total,
                total as libc::off_t,
            )
        };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            buf.truncate(0);
            return None;
        }
        if n == 0 {
            break;
        }
        total += n as usize;
    }
    buf.truncate(total);
    Some(total)
}

/// Reads a file relative to a directory fd (`openat`), e.g. `"1234/stat"` under
/// `/proc`. `rel` must be NUL-terminated. Returns bytes read into `buf`.
pub(crate) fn read_at(dirfd: libc::c_int, rel: &[u8], buf: &mut Vec<u8>) -> Option<usize> {
    debug_assert_eq!(rel.last(), Some(&0));
    let fd = unsafe {
        libc::openat(dirfd, rel.as_ptr() as *const libc::c_char, libc::O_RDONLY | libc::O_CLOEXEC)
    };
    if fd < 0 {
        return None;
    }
    let r = read_fd(fd, buf);
    unsafe { libc::close(fd) };
    r
}

fn read_fd(fd: libc::c_int, buf: &mut Vec<u8>) -> Option<usize> {
    if buf.capacity() < 4096 {
        buf.reserve(4096);
    }
    unsafe { buf.set_len(buf.capacity()) };
    let mut total = 0;
    loop {
        if total == buf.len() {
            let new_len = buf.len() * 2;
            buf.resize(new_len, 0);
        }
        let n = unsafe {
            libc::read(fd, buf.as_mut_ptr().add(total) as *mut libc::c_void, buf.len() - total)
        };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            buf.truncate(0);
            return None;
        }
        if n == 0 {
            break;
        }
        total += n as usize;
    }
    buf.truncate(total);
    Some(total)
}

/// Reads a small text file in one shot (used for discovery, not hot paths).
pub fn read_string(path: impl AsRef<Path>) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    Some(s.trim().to_string())
}

pub fn read_u64_file(path: impl AsRef<Path>) -> Option<u64> {
    parse_u64_strict(read_string(path)?.as_bytes())
}

/// Monotonic clock in microseconds. Same base as `g_get_monotonic_time()`,
/// so sample timestamps line up with the GTK frame clock.
pub fn now_us() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as i64 * 1_000_000 + ts.tv_nsec as i64 / 1000
}

pub fn clock_ticks() -> u64 {
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if t > 0 {
        t as u64
    } else {
        100
    }
}

pub fn page_size() -> u64 {
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 {
        p as u64
    } else {
        4096
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[inline]
pub fn trim(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = b {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}

/// Parses leading ASCII digits; stops at the first non-digit. Never fails.
#[inline]
pub fn parse_u64(b: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for &c in b {
        let d = c.wrapping_sub(b'0');
        if d > 9 {
            break;
        }
        v = v.wrapping_mul(10).wrapping_add(d as u64);
    }
    v
}

/// Parses a field that must be entirely digits.
pub fn parse_u64_strict(b: &[u8]) -> Option<u64> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(parse_u64(b))
}

#[inline]
pub fn parse_i64(b: &[u8]) -> i64 {
    match b.first() {
        Some(b'-') => -(parse_u64(&b[1..]) as i64),
        _ => parse_u64(b) as i64,
    }
}

/// Whitespace-separated fields of a line, without allocation.
#[inline]
pub fn fields(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    line.split(|c| *c == b' ' || *c == b'\t').filter(|f| !f.is_empty())
}

#[inline]
pub fn lines(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    buf.split(|c| *c == b'\n').filter(|l| !l.is_empty())
}

/// Lossy UTF-8 conversion that avoids allocating twice for valid input.
pub fn to_string_lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numbers() {
        assert_eq!(parse_u64(b"12345 rest"), 12345);
        assert_eq!(parse_u64(b""), 0);
        assert_eq!(parse_i64(b"-19"), -19);
        assert_eq!(parse_u64_strict(b"12a"), None);
        assert_eq!(parse_u64_strict(b"42"), Some(42));
        assert_eq!(trim(b"  42\n"), b"42");
    }

    #[test]
    fn kfile_rereads() {
        let mut f = KFile::open("/proc/self/stat");
        let a = f.read().map(|b| b.len());
        assert!(a.is_some());
        let mut f = KFile::open("/proc/uptime");
        let first = f.read().unwrap().to_vec();
        let second = f.read().unwrap().to_vec();
        assert!(!first.is_empty() && !second.is_empty());
        assert!(!KFile::open("/nonexistent/x").exists());
    }

    #[test]
    fn read_at_proc() {
        let dir = std::ffi::CString::new("/proc").unwrap();
        let fd = unsafe { libc::open(dir.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
        assert!(fd >= 0);
        let mut buf = Vec::new();
        let n = read_at(fd, b"self/stat\0", &mut buf).unwrap();
        assert!(n > 10);
        unsafe { libc::close(fd) };
    }
}
