//! NVIDIA GPUs through NVML, loaded at runtime with dlopen (no build-time
//! dependency, works with any driver version that ships libnvidia-ml.so.1).
//!
//! RTD3 (runtime D3) awareness: on Optimus laptops the dGPU powers off when
//! idle, but *any* NVML query keeps it awake (each call is a driver ioctl that
//! resets the autosuspend timer). So:
//!   * while `power/runtime_status` says "suspended" we never touch NVML;
//!   * when the GPU is awake we attach NVML and poll it;
//!   * after a few seconds of 0 % utilisation with no GPU processes we detach
//!     (nvmlShutdown) and let it power down;
//!   * we re-attach when someone else wakes it, or periodically if it stays
//!     awake on its own.

use super::GpuSample;
use crate::util::read_string;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const NVML_SUCCESS: c_int = 0;
const NVML_ERROR_INSUFFICIENT_SIZE: c_int = 7;
const NVML_TEMPERATURE_GPU: c_int = 0;
const NVML_CLOCK_GRAPHICS: c_int = 0;

/// Detach after this long at 0 % with no processes.
const IDLE_RELEASE_US: i64 = 3_000_000;
/// If the GPU stays awake without us, look again after this long.
const REATTACH_AFTER_US: i64 = 30_000_000;

type Device = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

type FnVoid = unsafe extern "C" fn() -> c_int;
type FnByPci = unsafe extern "C" fn(*const c_char, *mut Device) -> c_int;
type FnName = unsafe extern "C" fn(Device, *mut c_char, c_uint) -> c_int;
type FnUtil = unsafe extern "C" fn(Device, *mut Utilization) -> c_int;
type FnMem = unsafe extern "C" fn(Device, *mut Memory) -> c_int;
type FnUintArg = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> c_int;
type FnUint = unsafe extern "C" fn(Device, *mut c_uint) -> c_int;
type FnProcs = unsafe extern "C" fn(Device, *mut c_uint, *mut c_void) -> c_int;

struct Lib {
    init: FnVoid,
    shutdown: FnVoid,
    by_pci: FnByPci,
    name: FnName,
    util: FnUtil,
    mem: FnMem,
    temp: FnUintArg,
    clock: FnUintArg,
    max_clock: Option<FnUintArg>,
    power: Option<FnUint>,
    gfx_procs: Option<FnProcs>,
    compute_procs: Option<FnProcs>,
}

// Function pointers into a library that is never unloaded.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(load).as_ref()
}

fn load() -> Option<Lib> {
    let h = ["libnvidia-ml.so.1", "libnvidia-ml.so"].iter().find_map(|n| {
        let c = CString::new(*n).ok()?;
        let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        (!h.is_null()).then_some(h)
    })?;
    let sym = |names: &[&str]| -> Option<*mut c_void> {
        names.iter().find_map(|n| {
            let c = CString::new(*n).ok()?;
            let p = unsafe { libc::dlsym(h, c.as_ptr()) };
            (!p.is_null()).then_some(p)
        })
    };
    // SAFETY: signatures match the NVML C API (nvml.h) for these entry points.
    unsafe {
        Some(Lib {
            init: std::mem::transmute::<*mut c_void, FnVoid>(sym(&["nvmlInit_v2", "nvmlInit"])?),
            shutdown: std::mem::transmute::<*mut c_void, FnVoid>(sym(&["nvmlShutdown"])?),
            by_pci: std::mem::transmute::<*mut c_void, FnByPci>(sym(&[
                "nvmlDeviceGetHandleByPciBusId_v2",
                "nvmlDeviceGetHandleByPciBusId",
            ])?),
            name: std::mem::transmute::<*mut c_void, FnName>(sym(&["nvmlDeviceGetName"])?),
            util: std::mem::transmute::<*mut c_void, FnUtil>(sym(&["nvmlDeviceGetUtilizationRates"])?),
            mem: std::mem::transmute::<*mut c_void, FnMem>(sym(&["nvmlDeviceGetMemoryInfo"])?),
            temp: std::mem::transmute::<*mut c_void, FnUintArg>(sym(&["nvmlDeviceGetTemperature"])?),
            clock: std::mem::transmute::<*mut c_void, FnUintArg>(sym(&["nvmlDeviceGetClockInfo"])?),
            max_clock: sym(&["nvmlDeviceGetMaxClockInfo"]).map(|p| std::mem::transmute::<*mut c_void, FnUintArg>(p)),
            power: sym(&["nvmlDeviceGetPowerUsage"]).map(|p| std::mem::transmute::<*mut c_void, FnUint>(p)),
            gfx_procs: sym(&[
                "nvmlDeviceGetGraphicsRunningProcesses_v3",
                "nvmlDeviceGetGraphicsRunningProcesses_v2",
                "nvmlDeviceGetGraphicsRunningProcesses",
            ])
            .map(|p| std::mem::transmute::<*mut c_void, FnProcs>(p)),
            compute_procs: sym(&[
                "nvmlDeviceGetComputeRunningProcesses_v3",
                "nvmlDeviceGetComputeRunningProcesses_v2",
                "nvmlDeviceGetComputeRunningProcesses",
            ])
            .map(|p| std::mem::transmute::<*mut c_void, FnProcs>(p)),
        })
    }
}

/// nvmlInit/nvmlShutdown are reference counted by NVML itself, but we keep our
/// own count so several GPUs can attach/detach independently.
static SESSIONS: Mutex<usize> = Mutex::new(0);

fn session_open(l: &Lib) -> bool {
    let mut n = SESSIONS.lock().unwrap();
    if unsafe { (l.init)() } != NVML_SUCCESS {
        return false;
    }
    *n += 1;
    true
}

fn session_close(l: &Lib) {
    let mut n = SESSIONS.lock().unwrap();
    if *n > 0 {
        unsafe { (l.shutdown)() };
        *n -= 1;
    }
}

pub struct NvidiaGpu {
    bus_id: CString,
    /// RTD3 enabled (`power/control` == "auto"). If not, the GPU never sleeps
    /// and we can poll freely.
    rtd3: bool,
    device: Option<Device>,
    idle_since: Option<i64>,
    released_at: Option<i64>,
    slept_since_release: bool,
    name: Option<String>,
    mem_total: Option<u64>,
    max_clock: Option<u32>,
    attach_failed_at: Option<i64>,
}

// Device handles are plain integers/pointers owned by NVML; only used on the
// slow sampler thread.
unsafe impl Send for NvidiaGpu {}

impl NvidiaGpu {
    pub fn new(dev: &Path, pci_addr: &str) -> NvidiaGpu {
        let rtd3 = read_string(dev.join("power/control")).as_deref() == Some("auto");
        NvidiaGpu {
            bus_id: CString::new(pci_addr).unwrap_or_default(),
            rtd3,
            device: None,
            idle_since: None,
            released_at: None,
            slept_since_release: false,
            name: None,
            mem_total: None,
            max_clock: None,
            attach_failed_at: None,
        }
    }

    pub(super) fn on_suspended(&mut self) {
        self.slept_since_release = true;
        if self.device.is_some() {
            self.detach(None);
        }
    }

    fn attach(&mut self) -> bool {
        let Some(l) = lib() else { return false };
        if !session_open(l) {
            return false;
        }
        let mut d: Device = std::ptr::null_mut();
        if unsafe { (l.by_pci)(self.bus_id.as_ptr(), &mut d) } != NVML_SUCCESS || d.is_null() {
            session_close(l);
            return false;
        }
        if self.name.is_none() {
            let mut buf = [0 as c_char; 96];
            if unsafe { (l.name)(d, buf.as_mut_ptr(), buf.len() as c_uint) } == NVML_SUCCESS {
                let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
                self.name = Some(s);
            }
            if let Some(f) = l.max_clock {
                let mut v = 0;
                if unsafe { f(d, NVML_CLOCK_GRAPHICS, &mut v) } == NVML_SUCCESS {
                    self.max_clock = Some(v);
                }
            }
        }
        self.device = Some(d);
        self.idle_since = None;
        true
    }

    fn detach(&mut self, now_us: Option<i64>) {
        if self.device.take().is_some() {
            if let Some(l) = lib() {
                session_close(l);
            }
        }
        self.released_at = now_us;
        self.slept_since_release = false;
    }

    pub(super) fn sample(&mut self, now_us: i64, out: &mut GpuSample) {
        out.driver_name = self.name.clone();
        out.mem_total = self.mem_total;
        out.freq_max_mhz = self.max_clock;
        if self.device.is_none() {
            let should_attach = !self.rtd3
                || self.slept_since_release
                || match self.released_at {
                    None => true,
                    Some(t) => now_us - t > REATTACH_AFTER_US,
                };
            let backoff_over = self.attach_failed_at.map_or(true, |t| now_us - t > REATTACH_AFTER_US);
            let attached = should_attach && backoff_over && {
                let ok = self.attach();
                self.attach_failed_at = if ok { None } else { Some(now_us) };
                ok
            };
            if !attached {
                // Awake for someone else's reasons; report idle without polling.
                out.polled = false;
                out.busy = Some(0.0);
                out.freq_mhz = None;
                out.power_w = None;
                return;
            }
        }
        let (Some(l), Some(d)) = (lib(), self.device) else { return };
        let mut u = Utilization::default();
        let ok = unsafe { (l.util)(d, &mut u) } == NVML_SUCCESS;
        out.busy = ok.then(|| (u.gpu as f32 / 100.0).clamp(0.0, 1.0));
        let mut m = Memory::default();
        if unsafe { (l.mem)(d, &mut m) } == NVML_SUCCESS {
            out.mem_used = Some(m.used);
            out.mem_total = Some(m.total);
            self.mem_total = Some(m.total);
        }
        let mut v: c_uint = 0;
        out.temp_c = (unsafe { (l.temp)(d, NVML_TEMPERATURE_GPU, &mut v) } == NVML_SUCCESS).then_some(v as f32);
        out.freq_mhz = (unsafe { (l.clock)(d, NVML_CLOCK_GRAPHICS, &mut v) } == NVML_SUCCESS).then_some(v);
        out.power_w = l
            .power
            .and_then(|f| (unsafe { f(d, &mut v) } == NVML_SUCCESS).then_some(v as f32 / 1000.0));
        out.driver_name = self.name.clone();
        out.polled = true;

        if self.rtd3 {
            let busy = u.gpu > 0 || has_procs(l.gfx_procs, d) || has_procs(l.compute_procs, d);
            if busy {
                self.idle_since = None;
            } else {
                let since = *self.idle_since.get_or_insert(now_us);
                if now_us - since > IDLE_RELEASE_US {
                    self.detach(Some(now_us));
                }
            }
        }
    }
}

impl Drop for NvidiaGpu {
    fn drop(&mut self) {
        self.detach(None);
    }
}

fn has_procs(f: Option<FnProcs>, d: Device) -> bool {
    let Some(f) = f else { return false };
    let mut count: c_uint = 0;
    // With *count == 0 NVML returns INSUFFICIENT_SIZE iff processes exist.
    let r = unsafe { f(d, &mut count, std::ptr::null_mut()) };
    r == NVML_ERROR_INSUFFICIENT_SIZE || (r == NVML_SUCCESS && count > 0)
}
