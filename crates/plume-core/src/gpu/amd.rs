//! AMD GPUs via amdgpu sysfs (no ROCm needed).

use super::{first_existing, hwmon_dir, GpuSample};
use crate::util::KFile;
use std::path::Path;

pub struct AmdGpu {
    busy: KFile,
    vram_used: KFile,
    vram_total: Option<u64>,
    temp: Option<KFile>,
    freq: Option<KFile>,
    power: Option<KFile>,
}

impl AmdGpu {
    pub fn new(dev: &Path) -> AmdGpu {
        let hw = hwmon_dir(dev);
        let hwf = |names: &[&str]| -> Option<KFile> {
            let hw = hw.as_ref()?;
            first_existing(&names.iter().map(|n| hw.join(n)).collect::<Vec<_>>())
        };
        AmdGpu {
            busy: KFile::open(dev.join("gpu_busy_percent")),
            vram_used: KFile::open(dev.join("mem_info_vram_used")),
            vram_total: KFile::open(dev.join("mem_info_vram_total")).read_u64(),
            temp: hwf(&["temp1_input"]),
            freq: hwf(&["freq1_input"]),
            power: hwf(&["power1_average", "power1_input"]),
        }
    }

    pub fn sample(&mut self, out: &mut GpuSample) {
        out.busy = self.busy.read_u64().map(|p| (p as f32 / 100.0).clamp(0.0, 1.0));
        out.mem_used = self.vram_used.read_u64();
        out.mem_total = self.vram_total;
        out.temp_c = self.temp.as_mut().and_then(|f| f.read_u64()).map(|m| m as f32 / 1000.0);
        out.freq_mhz = self.freq.as_mut().and_then(|f| f.read_u64()).map(|hz| (hz / 1_000_000) as u32);
        out.power_w = self.power.as_mut().and_then(|f| f.read_u64()).map(|uw| uw as f32 / 1e6);
    }
}
