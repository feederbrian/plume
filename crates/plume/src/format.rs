//! Human-readable formatting (IEC units, like GNOME System Monitor).

pub fn bytes(b: u64) -> String {
    bytes_f(b as f64)
}

pub fn bytes_f(b: f64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", U[i])
    } else if v < 10.0 {
        format!("{v:.2} {}", U[i])
    } else if v < 100.0 {
        format!("{v:.1} {}", U[i])
    } else {
        format!("{v:.0} {}", U[i])
    }
}

pub fn rate(bps: f64) -> String {
    format!("{}/s", bytes_f(bps))
}

/// Compact rate for tight labels: "1.2M", "340K".
pub fn rate_short(bps: f64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bps.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v < 0.5 && i == 0 {
        "0".into()
    } else if v < 10.0 && i > 0 {
        format!("{v:.1}{}", U[i])
    } else {
        format!("{v:.0}{}", U[i])
    }
}

pub fn percent(frac: f32) -> String {
    let p = frac * 100.0;
    if p < 10.0 && p > 0.0 {
        format!("{p:.1} %")
    } else {
        format!("{p:.0} %")
    }
}

pub fn duration(secs: f64) -> String {
    let s = secs as u64;
    let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", s % 60)
    }
}

pub fn mhz(m: u32) -> String {
    if m >= 1000 {
        format!("{:.2} GHz", m as f64 / 1000.0)
    } else {
        format!("{m} MHz")
    }
}

pub fn cpu_time(ticks: u64, hz: u64) -> String {
    let cs = ticks * 100 / hz.max(1);
    let (h, m, s, c) = (cs / 360000, (cs / 6000) % 60, (cs / 100) % 60, cs % 100);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}.{c:02}")
    }
}

pub fn state(s: u8) -> &'static str {
    match s {
        b'R' => "Running",
        b'S' => "Sleeping",
        b'D' => "Disk wait",
        b'Z' => "Zombie",
        b'T' => "Stopped",
        b't' => "Traced",
        b'I' => "Idle",
        b'X' => "Dead",
        _ => "Unknown",
    }
}

/// Rounds an axis maximum up to 1/2/5 × 10^k in "byte" space.
pub fn nice_ceiling(v: f32) -> f32 {
    if v <= 0.0 {
        return 1.0;
    }
    let exp = v.log2().floor();
    let base = 2f32.powf(exp);
    for m in [1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0] {
        if base * m >= v {
            return base * m;
        }
    }
    base * 8.0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fmt() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.50 KiB");
        assert_eq!(bytes(16_000_000_000), "14.9 GiB");
        assert_eq!(percent(0.234), "23 %");
        assert_eq!(percent(0.034), "3.4 %");
        assert_eq!(mhz(2600), "2.60 GHz");
        assert_eq!(duration(3725.0), "1h 2m");
        assert!(nice_ceiling(3000.0) >= 3000.0);
        assert_eq!(cpu_time(12345, 100), "2:03.45");
    }
}
