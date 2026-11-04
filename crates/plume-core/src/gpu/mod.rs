//! GPU discovery and sampling.
//!
//! Every backend is gated on the PCI runtime-PM state first: reading most GPU
//! counters takes a runtime-PM reference and *wakes a sleeping GPU*. On a
//! hybrid laptop (Intel iGPU + NVIDIA dGPU) the discrete GPU spends most of its
//! life in D3cold; a monitor that polls it naively keeps it powered and costs
//! battery. We read `power/runtime_status` (free, never wakes the device) and
//! only touch the driver when the GPU is already awake.

pub mod amd;
pub mod drm;
pub mod intel;
pub mod nvidia;
mod pciids;

use crate::util::{read_string, KFile};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Nvidia,
    Amd,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PowerState {
    #[default]
    Unknown,
    Active,
    /// Runtime-suspended (D3). Not queried, so it stays asleep.
    Suspended,
}

#[derive(Clone, Debug)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: Vendor,
    pub driver: String,
    pub pci_addr: String,
    pub integrated: bool,
    /// Human-readable description of where the utilisation figure comes from.
    pub busy_source: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuSample {
    pub power: PowerState,
    /// Fraction 0..1. `None` = not measurable right now.
    pub busy: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub freq_mhz: Option<u32>,
    pub freq_max_mhz: Option<u32>,
    pub temp_c: Option<f32>,
    pub power_w: Option<f32>,
    /// False while we deliberately stop polling so the GPU can power down.
    pub polled: bool,
    /// Intel: share of time out of RC6 sleep ("awake"), which overstates
    /// load at steady low activity; `busy` prefers real engine time.
    pub awake: Option<f32>,
    /// Per-engine-class busy fractions from DRM fdinfo, when available.
    pub engines: Vec<(String, f32)>,
    /// Name reported by the driver, if better than the PCI database's.
    pub driver_name: Option<String>,
}

enum Backend {
    I915(intel::I915),
    Xe(intel::Xe),
    Nvidia(nvidia::NvidiaGpu),
    Amd(amd::AmdGpu),
    Unsupported,
}

pub struct Gpu {
    pub info: GpuInfo,
    runtime_status: KFile,
    backend: Backend,
}

impl Gpu {
    /// NVML calls are comparatively slow (ioctls, occasional ~50 ms init),
    /// so NVIDIA GPUs are sampled on the slow worker, everything else on the
    /// fast one.
    pub fn is_slow(&self) -> bool {
        matches!(self.backend, Backend::Nvidia(_))
    }

    pub fn sample(&mut self, now_us: i64, out: &mut GpuSample) {
        let power = match self.runtime_status.read() {
            Some(b) => match crate::util::trim(b) {
                b"suspended" | b"suspending" => PowerState::Suspended,
                _ => PowerState::Active,
            },
            None => PowerState::Active,
        };
        out.power = power;
        if power == PowerState::Suspended {
            out.busy = Some(0.0);
            out.freq_mhz = None;
            out.power_w = None;
            out.temp_c = None;
            out.polled = false;
            if let Backend::Nvidia(n) = &mut self.backend {
                n.on_suspended();
            }
            return;
        }
        out.polled = true;
        match &mut self.backend {
            Backend::I915(b) => b.sample(now_us, out),
            Backend::Xe(b) => b.sample(now_us, out),
            Backend::Amd(b) => b.sample(out),
            Backend::Nvidia(b) => b.sample(now_us, out),
            Backend::Unsupported => out.busy = None,
        }
    }
}

/// Finds display-class PCI devices under `root` (normally `/`).
pub fn discover(root: &Path) -> Vec<Gpu> {
    let base = root.join("sys/bus/pci/devices");
    let Ok(rd) = std::fs::read_dir(&base) else { return Vec::new() };
    let mut devs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    devs.sort();
    let mut out = Vec::new();
    for dev in devs {
        let class = read_string(dev.join("class")).and_then(|s| parse_hex(&s)).unwrap_or(0);
        if class >> 16 != 0x03 {
            continue;
        }
        let vendor_id = read_string(dev.join("vendor")).and_then(|s| parse_hex(&s)).unwrap_or(0) as u16;
        let device_id = read_string(dev.join("device")).and_then(|s| parse_hex(&s)).unwrap_or(0) as u16;
        let driver = std::fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        if driver.is_empty() {
            continue; // unbound device (e.g. vfio passthrough); nothing to read
        }
        let pci_addr = dev.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let vendor = match vendor_id {
            0x8086 => Vendor::Intel,
            0x10de => Vendor::Nvidia,
            0x1002 => Vendor::Amd,
            _ => Vendor::Other,
        };
        let card = find_card(&dev);
        let (backend, busy_source) = match (driver.as_str(), &card) {
            ("i915", Some(card)) => match intel::I915::new(card) {
                Some(b) => (Backend::I915(b), "time outside RC6 sleep"),
                None => (Backend::Unsupported, "unavailable"),
            },
            ("xe", _) => match intel::Xe::new(&dev) {
                Some(b) => (Backend::Xe(b), "time outside idle state"),
                None => (Backend::Unsupported, "unavailable"),
            },
            ("nvidia", _) => (Backend::Nvidia(nvidia::NvidiaGpu::new(&dev, &pci_addr)), "NVML"),
            ("amdgpu", _) => (Backend::Amd(amd::AmdGpu::new(&dev)), "gpu_busy_percent"),
            _ => (Backend::Unsupported, "unsupported driver"),
        };
        let bus = pci_addr.split(':').nth(1).unwrap_or("");
        let integrated = vendor == Vendor::Intel && bus == "00";
        let name = pciids::lookup(vendor_id, device_id)
            .unwrap_or_else(|| match vendor {
                Vendor::Intel => "Intel Graphics".into(),
                Vendor::Nvidia => "NVIDIA GPU".into(),
                Vendor::Amd => "AMD GPU".into(),
                Vendor::Other => format!("GPU {vendor_id:04x}:{device_id:04x}"),
            });
        out.push(Gpu {
            info: GpuInfo { name, vendor, driver, pci_addr, integrated, busy_source },
            runtime_status: KFile::open(dev.join("power/runtime_status")),
            backend,
        });
    }
    // Integrated first, then by PCI address (already sorted).
    out.sort_by_key(|g| !g.info.integrated);
    out
}

fn find_card(dev: &Path) -> Option<PathBuf> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir(dev.join("drm"))
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("card") && !n.contains('-')
        })
        .map(|e| e.path())
        .collect();
    cards.sort();
    cards.into_iter().next()
}

pub(crate) fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

pub(crate) fn first_existing(paths: &[PathBuf]) -> Option<KFile> {
    paths.iter().map(KFile::open).find(KFile::exists)
}

pub(crate) fn hwmon_dir(dev: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dev.join("hwmon")).ok()?.flatten().map(|e| e.path()).next()
}

#[cfg(test)]
mod tests;
