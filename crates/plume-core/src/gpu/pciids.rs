//! GPU marketing names from the system PCI ID database (pci.ids).

use std::io::{BufRead, BufReader};

const PATHS: [&str; 3] = ["/usr/share/misc/pci.ids", "/usr/share/hwdata/pci.ids", "/usr/share/pci.ids"];

/// "Intel UHD Graphics 630", "NVIDIA GeForce RTX 2080 Mobile", ...
pub fn lookup(vendor: u16, device: u16) -> Option<String> {
    let f = PATHS.iter().find_map(|p| std::fs::File::open(p).ok())?;
    lookup_in(BufReader::new(f), vendor, device)
}

pub(crate) fn lookup_in(r: impl BufRead, vendor: u16, device: u16) -> Option<String> {
    let vhex = format!("{vendor:04x}");
    let dhex = format!("{device:04x}");
    let mut vendor_name: Option<String> = None;
    for line in r.lines().map_while(Result::ok) {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if vendor_name.is_some() {
                break; // left our vendor's block
            }
            if line.starts_with(&vhex) {
                vendor_name = Some(line[4..].trim().to_string());
            }
            continue;
        }
        let Some(vn) = &vendor_name else { continue };
        if line.starts_with("\t\t") {
            continue; // subsystem
        }
        let l = &line[1..];
        if l.starts_with(&dhex) {
            return Some(pretty(vn, l[4..].trim()));
        }
    }
    None
}

fn pretty(vendor: &str, device: &str) -> String {
    let v = if vendor.starts_with("Intel") {
        "Intel"
    } else if vendor.starts_with("NVIDIA") {
        "NVIDIA"
    } else if vendor.starts_with("Advanced Micro Devices") || vendor.contains("AMD") {
        "AMD"
    } else {
        vendor.split_whitespace().next().unwrap_or(vendor)
    };
    // Prefer the bracketed marketing name: "TU104M [GeForce RTX 2080 Mobile]".
    let d = match (device.rfind('['), device.rfind(']')) {
        (Some(a), Some(b)) if b > a => &device[a + 1..b],
        _ => device,
    };
    if d.starts_with(v) { d.to_string() } else { format!("{v} {d}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DB: &str = "# comment\n8086  Intel Corporation\n\t3e9b  CoffeeLake-H GT2 [UHD Graphics 630]\n\t\t1028 0950  Subsys\n\
10de  NVIDIA Corporation\n\t1ed0  TU104BM [GeForce RTX 2080 Mobile]\n\t1f91  TU117M [GeForce GTX 1650 Mobile / Max-Q]\n\
1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t73bf  Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]\n";

    #[test]
    fn names() {
        let q = |v, d| lookup_in(DB.as_bytes(), v, d);
        assert_eq!(q(0x8086, 0x3e9b).as_deref(), Some("Intel UHD Graphics 630"));
        assert_eq!(q(0x10de, 0x1ed0).as_deref(), Some("NVIDIA GeForce RTX 2080 Mobile"));
        assert_eq!(q(0x1002, 0x73bf).as_deref(), Some("AMD Radeon RX 6800/6800 XT / 6900 XT"));
        assert_eq!(q(0x10de, 0xffff), None);
    }
}
