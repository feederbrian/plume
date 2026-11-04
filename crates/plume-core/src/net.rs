//! Network throughput from `/proc/net/dev`.

use crate::util::{fields, lines, parse_u64, KFile};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Interface {
    pub name: String,
    /// Backed by a real device (has `/sys/class/net/<if>/device`).
    pub physical: bool,
    pub wireless: bool,
    pub rx_total: u64,
    pub tx_total: u64,
    /// Bytes per second.
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_pps: f64,
    pub tx_pps: f64,
    seen: bool,
}

#[derive(Clone, Debug, Default)]
pub struct NetSample {
    /// Aggregate over counted interfaces, bytes per second.
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    /// Packets per second / totals over counted interfaces.
    pub rx_pps: f64,
    pub tx_pps: f64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub interfaces: Vec<Interface>,
}

pub struct NetReader {
    file: KFile,
    sys_net: PathBuf,
    ifaces: Vec<Interface>,
    last_us: i64,
}

impl NetReader {
    pub fn new() -> NetReader {
        Self::with_root(Path::new("/"))
    }
    pub fn with_root(root: &Path) -> NetReader {
        NetReader {
            file: KFile::open(root.join("proc/net/dev")),
            sys_net: root.join("sys/class/net"),
            ifaces: Vec::new(),
            last_us: 0,
        }
    }

    /// `now_us` is a monotonic timestamp (see `util::now_us`).
    pub fn sample(&mut self, now_us: i64, out: &mut NetSample) -> bool {
        let Some(buf) = self.file.read() else { return false };
        let dt = if self.last_us > 0 { (now_us - self.last_us) as f64 / 1e6 } else { 0.0 };
        for i in &mut self.ifaces {
            i.seen = false;
        }
        for line in lines(buf).skip(2) {
            let Some(colon) = line.iter().position(|&c| c == b':') else { continue };
            let name = crate::util::trim(&line[..colon]);
            let mut f = fields(&line[colon + 1..]);
            let rx = f.next().map(parse_u64).unwrap_or(0);
            let rxp = f.next().map(parse_u64).unwrap_or(0);
            let tx = f.nth(6).map(parse_u64).unwrap_or(0);
            let txp = f.next().map(parse_u64).unwrap_or(0);
            let idx = match self.ifaces.iter().position(|i| i.name.as_bytes() == name) {
                Some(i) => i,
                None => {
                    let name = String::from_utf8_lossy(name).into_owned();
                    let dir = self.sys_net.join(&name);
                    self.ifaces.push(Interface {
                        physical: dir.join("device").exists(),
                        wireless: dir.join("wireless").exists() || dir.join("phy80211").exists(),
                        name,
                        rx_total: rx,
                        tx_total: tx,
                        rx_packets: rxp,
                        tx_packets: txp,
                        ..Default::default()
                    });
                    self.ifaces.len() - 1
                }
            };
            let i = &mut self.ifaces[idx];
            if dt > 0.0 {
                i.rx_rate = rx.saturating_sub(i.rx_total) as f64 / dt;
                i.tx_rate = tx.saturating_sub(i.tx_total) as f64 / dt;
                i.rx_pps = rxp.saturating_sub(i.rx_packets) as f64 / dt;
                i.tx_pps = txp.saturating_sub(i.tx_packets) as f64 / dt;
            }
            i.rx_total = rx;
            i.tx_total = tx;
            i.rx_packets = rxp;
            i.tx_packets = txp;
            i.seen = true;
        }
        self.ifaces.retain(|i| i.seen);
        self.last_us = now_us;

        let any_physical = self.ifaces.iter().any(|i| i.physical);
        let counted = |i: &&Interface| {
            i.name != "lo" && if any_physical { i.physical } else { !is_virtual(&i.name) }
        };
        out.rx_rate = self.ifaces.iter().filter(counted).map(|i| i.rx_rate).sum();
        out.tx_rate = self.ifaces.iter().filter(counted).map(|i| i.tx_rate).sum();
        out.rx_total = self.ifaces.iter().filter(counted).map(|i| i.rx_total).sum();
        out.tx_total = self.ifaces.iter().filter(counted).map(|i| i.tx_total).sum();
        out.rx_pps = self.ifaces.iter().filter(counted).map(|i| i.rx_pps).sum();
        out.tx_pps = self.ifaces.iter().filter(counted).map(|i| i.tx_pps).sum();
        out.rx_packets = self.ifaces.iter().filter(counted).map(|i| i.rx_packets).sum();
        out.tx_packets = self.ifaces.iter().filter(counted).map(|i| i.tx_packets).sum();
        out.interfaces.clone_from(&self.ifaces);
        dt > 0.0
    }
}

impl Default for NetReader {
    fn default() -> Self {
        Self::new()
    }
}

fn is_virtual(name: &str) -> bool {
    const PREFIXES: [&str; 8] = ["veth", "docker", "br-", "virbr", "vnet", "lxc", "tailscale", "zt"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rates_from_fixture() {
        let root = std::env::temp_dir().join(format!("plume-net-{}", std::process::id()));
        fs::create_dir_all(root.join("proc/net")).unwrap();
        fs::create_dir_all(root.join("sys/class/net/wlp0s20f3/device")).unwrap();
        fs::create_dir_all(root.join("sys/class/net/wlp0s20f3/wireless")).unwrap();
        fs::create_dir_all(root.join("sys/class/net/lo")).unwrap();
        let hdr = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n";
        let write = |rx: u64, tx: u64| {
            fs::write(
                root.join("proc/net/dev"),
                format!("{hdr}    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\nwlp0s20f3: {rx} 10 0 0 0 0 0 0 {tx} 10 0 0 0 0 0 0\n"),
            )
            .unwrap();
        };
        write(1000, 500);
        let mut r = NetReader::with_root(&root);
        let mut s = NetSample::default();
        assert!(!r.sample(1_000_000, &mut s));
        write(3000, 1500);
        assert!(r.sample(1_500_000, &mut s));
        assert_eq!(s.rx_rate, 4000.0);
        assert_eq!(s.tx_rate, 2000.0);
        assert_eq!(s.rx_pps, 0.0);
        let w = s.interfaces.iter().find(|i| i.name == "wlp0s20f3").unwrap();
        assert!(w.physical && w.wireless);
        fs::remove_dir_all(root).ok();
    }
}
