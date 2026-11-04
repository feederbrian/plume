//! Discovery and sampling against a fake sysfs tree shaped like an
//! i7-9750H laptop: Intel UHD 630 (i915) + RTX 2080 Max-Q (nvidia, RTD3).

use super::*;
use std::fs;
use std::os::unix::fs::symlink;

fn fake_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("plume-gpu-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = fs::remove_dir_all(&root);
    let pci = root.join("sys/bus/pci/devices");
    let drivers = root.join("sys/bus/pci/drivers");
    // Intel iGPU
    let igpu = pci.join("0000:00:02.0");
    fs::create_dir_all(igpu.join("drm/card1/gt/gt0")).unwrap();
    fs::create_dir_all(igpu.join("drm/card1-eDP-1")).unwrap();
    fs::create_dir_all(igpu.join("power")).unwrap();
    fs::create_dir_all(drivers.join("i915")).unwrap();
    symlink(drivers.join("i915"), igpu.join("driver")).unwrap();
    fs::write(igpu.join("class"), "0x030000\n").unwrap();
    fs::write(igpu.join("vendor"), "0x8086\n").unwrap();
    fs::write(igpu.join("device"), "0x3e9b\n").unwrap();
    fs::write(igpu.join("power/runtime_status"), "active\n").unwrap();
    fs::write(igpu.join("drm/card1/gt/gt0/rc6_residency_ms"), "1000\n").unwrap();
    fs::write(igpu.join("drm/card1/gt/gt0/rps_act_freq_mhz"), "350\n").unwrap();
    fs::write(igpu.join("drm/card1/gt/gt0/rps_RP0_freq_mhz"), "1150\n").unwrap();
    // NVIDIA dGPU, runtime-suspended
    let dgpu = pci.join("0000:01:00.0");
    fs::create_dir_all(dgpu.join("power")).unwrap();
    fs::create_dir_all(drivers.join("nvidia")).unwrap();
    symlink(drivers.join("nvidia"), dgpu.join("driver")).unwrap();
    fs::write(dgpu.join("class"), "0x030000\n").unwrap();
    fs::write(dgpu.join("vendor"), "0x10de\n").unwrap();
    fs::write(dgpu.join("device"), "0x1ed0\n").unwrap();
    fs::write(dgpu.join("power/runtime_status"), "suspended\n").unwrap();
    fs::write(dgpu.join("power/control"), "auto\n").unwrap();
    // Unrelated non-display device
    let nvme = pci.join("0000:02:00.0");
    fs::create_dir_all(&nvme).unwrap();
    fs::write(nvme.join("class"), "0x010802\n").unwrap();
    root
}

#[test]
fn discovers_hybrid_laptop() {
    let root = fake_root();
    let gpus = discover(&root);
    assert_eq!(gpus.len(), 2);
    assert_eq!(gpus[0].info.vendor, Vendor::Intel);
    assert!(gpus[0].info.integrated);
    assert_eq!(gpus[0].info.driver, "i915");
    assert!(!gpus[0].is_slow());
    assert_eq!(gpus[1].info.vendor, Vendor::Nvidia);
    assert!(!gpus[1].info.integrated);
    assert!(gpus[1].is_slow());
    fs::remove_dir_all(root).ok();
}

#[test]
fn intel_busy_from_rc6() {
    let root = fake_root();
    let mut gpus = discover(&root);
    let mut s = GpuSample::default();
    gpus[0].sample(1_000_000, &mut s);
    assert_eq!(s.power, PowerState::Active);
    assert_eq!(s.freq_mhz, Some(350));
    assert_eq!(s.freq_max_mhz, Some(1150));
    // 200 ms later the GPU spent 150 ms in RC6 → 25 % busy. fs::write
    // truncates in place (same inode), so the open KFile sees the new value.
    let rc6 = root.join("sys/bus/pci/devices/0000:00:02.0/drm/card1/gt/gt0/rc6_residency_ms");
    fs::write(&rc6, "1150\n").unwrap();
    gpus[0].sample(1_200_000, &mut s);
    let b = s.busy.expect("busy");
    assert!((b - 0.25).abs() < 1e-3, "busy = {b}");
    fs::remove_dir_all(root).ok();
}

#[test]
fn suspended_dgpu_is_not_polled() {
    let root = fake_root();
    let mut gpus = discover(&root);
    let mut s = GpuSample::default();
    gpus[1].sample(1_000_000, &mut s);
    assert_eq!(s.power, PowerState::Suspended);
    assert!(!s.polled);
    assert_eq!(s.busy, Some(0.0));
    fs::remove_dir_all(root).ok();
}
