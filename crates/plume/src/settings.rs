//! User settings, persisted as a small INI file (~/.config/plume/plume.ini).
//! GKeyFile instead of GSettings so the app runs without installing a schema.

use gtk::glib;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    pub theme: String,
    pub sample_hz: f64,
    pub proc_interval_ms: u64,
    pub history_secs: u32,
    /// 0 = follow the display refresh rate.
    pub fps_cap: u32,
    /// Frame rate for unfocused windows and floating readouts.
    pub fps_cap_bg: u32,
    /// Graph line width in device pixels.
    pub line_px: f64,
    /// Scrolling graphs redraw once they have moved this many device pixels.
    pub motion_step: f64,
    /// True-black backgrounds for dark themes (OLED panels).
    pub oled: bool,
    pub smooth: bool,
    pub cpu_per_core: bool,
    pub divide_cpu: bool,
    pub show_kernel_threads: bool,
    pub proc_filter: String,
    pub confirm_kill: bool,
    pub float_opacity: f64,
    pub floats: Vec<String>,
    pub page: String,
    pub width: i32,
    pub height: i32,
    /// Per-tab sort: tab id → "column:desc|asc".
    pub sorts: BTreeMap<String, String>,
    /// Per-float time scale in seconds (float id → secs).
    pub float_scales: BTreeMap<String, u32>,
    /// Per-float window size set by the user (float id → width, height).
    pub float_sizes: BTreeMap<String, (i32, i32)>,
    /// Overview card order (card ids); cards not listed keep their default place.
    pub card_order: Vec<String>,
    /// The CPU floating readout shows every logical CPU instead of the total.
    pub float_cpu_logical: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "system".into(),
            sample_hz: 10.0,
            proc_interval_ms: 1000,
            history_secs: 60,
            fps_cap: 60,
            fps_cap_bg: 30,
            line_px: 1.5,
            motion_step: 1.0,
            oled: false,
            smooth: false,
            cpu_per_core: false,
            divide_cpu: true,
            show_kernel_threads: false,
            proc_filter: "all".into(),
            confirm_kill: true,
            float_opacity: 0.92,
            floats: Vec::new(),
            page: "cpu".into(),
            width: 1000,
            height: 720,
            sorts: BTreeMap::new(),
            float_scales: BTreeMap::new(),
            float_sizes: BTreeMap::new(),
            card_order: Vec::new(),
            float_cpu_logical: false,
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Settings> = RefCell::new(load());
}

fn path() -> PathBuf {
    glib::user_config_dir().join("plume").join("plume.ini")
}

const G: &str = "plume";

fn load() -> Settings {
    let mut s = Settings::default();
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return s;
    }
    let str_ = |k: &str| kf.string(G, k).ok().map(|v| v.to_string());
    let int = |k: &str| kf.integer(G, k).ok();
    let dbl = |k: &str| kf.double(G, k).ok();
    let boo = |k: &str| kf.boolean(G, k).ok();
    if let Some(v) = str_("theme") { s.theme = v; }
    if let Some(v) = dbl("sample_hz") { s.sample_hz = v.clamp(1.0, 60.0); }
    if let Some(v) = int("proc_interval_ms") { s.proc_interval_ms = v.clamp(250, 10_000) as u64; }
    if let Some(v) = int("history_secs") { s.history_secs = v.clamp(10, 3600) as u32; }
    if let Some(v) = int("fps_cap") { s.fps_cap = v.clamp(0, 240) as u32; }
    if let Some(v) = int("fps_cap_bg") { s.fps_cap_bg = v.clamp(0, 240) as u32; }
    if let Some(v) = dbl("line_px") { s.line_px = v.clamp(0.5, 6.0); }
    if let Some(v) = dbl("motion_step") { s.motion_step = v.clamp(0.1, 8.0); }
    if let Some(v) = boo("oled") { s.oled = v; }
    if let Some(v) = boo("smooth") { s.smooth = v; }
    if let Some(v) = boo("cpu_per_core") { s.cpu_per_core = v; }
    if let Some(v) = boo("divide_cpu") { s.divide_cpu = v; }
    if let Some(v) = boo("show_kernel_threads") { s.show_kernel_threads = v; }
    if let Some(v) = str_("proc_filter") { s.proc_filter = v; }
    if let Some(v) = boo("confirm_kill") { s.confirm_kill = v; }
    if let Some(v) = boo("float_cpu_logical") { s.float_cpu_logical = v; }
    if let Some(v) = dbl("float_opacity") { s.float_opacity = v.clamp(0.3, 1.0); }
    if let Some(v) = str_("floats") {
        s.floats = v.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect();
    }
    if let Some(v) = str_("card_order") {
        s.card_order = v.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect();
    }
    if let Some(v) = str_("page") {
        // 0.1 page names → tabs
        s.page = match v.as_str() {
            "processes" => "cpu".into(),
            "resources" => "overview".into(),
            _ => v,
        };
    }
    if let Some(v) = int("width") { s.width = v.max(400); }
    if let Some(v) = int("height") { s.height = v.max(300); }
    if let Ok(keys) = kf.keys(G) {
        for k in keys.iter() {
            if let Some(tab) = k.strip_prefix("sortby_") {
                if let Some(v) = str_(k) {
                    s.sorts.insert(tab.to_string(), v);
                }
            }
            if let Some(id) = k.strip_prefix("floatscale_") {
                if let Some(v) = int(k) {
                    s.float_scales.insert(id.to_string(), v.clamp(10, 3600) as u32);
                }
            }
            if let Some(id) = k.strip_prefix("floatsize_") {
                let size = str_(k).and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some((w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?))
                });
                if let Some((w, h)) = size {
                    s.float_sizes.insert(id.to_string(), (w.clamp(48, 8192), h.clamp(32, 8192)));
                }
            }
        }
    }
    s
}

fn save(s: &Settings) {
    let kf = glib::KeyFile::new();
    kf.set_string(G, "theme", &s.theme);
    kf.set_double(G, "sample_hz", s.sample_hz);
    kf.set_integer(G, "proc_interval_ms", s.proc_interval_ms as i32);
    kf.set_integer(G, "history_secs", s.history_secs as i32);
    kf.set_integer(G, "fps_cap", s.fps_cap as i32);
    kf.set_integer(G, "fps_cap_bg", s.fps_cap_bg as i32);
    kf.set_double(G, "line_px", s.line_px);
    kf.set_double(G, "motion_step", s.motion_step);
    kf.set_boolean(G, "oled", s.oled);
    kf.set_boolean(G, "smooth", s.smooth);
    kf.set_boolean(G, "cpu_per_core", s.cpu_per_core);
    kf.set_boolean(G, "divide_cpu", s.divide_cpu);
    kf.set_boolean(G, "show_kernel_threads", s.show_kernel_threads);
    kf.set_string(G, "proc_filter", &s.proc_filter);
    kf.set_boolean(G, "confirm_kill", s.confirm_kill);
    kf.set_boolean(G, "float_cpu_logical", s.float_cpu_logical);
    kf.set_double(G, "float_opacity", s.float_opacity);
    kf.set_string(G, "floats", &s.floats.join(","));
    kf.set_string(G, "card_order", &s.card_order.join(","));
    kf.set_string(G, "page", &s.page);
    kf.set_integer(G, "width", s.width);
    kf.set_integer(G, "height", s.height);
    for (tab, v) in &s.sorts {
        kf.set_string(G, &format!("sortby_{tab}"), v);
    }
    for (id, v) in &s.float_scales {
        kf.set_integer(G, &format!("floatscale_{id}"), *v as i32);
    }
    for (id, (w, h)) in &s.float_sizes {
        kf.set_string(G, &format!("floatsize_{id}"), &format!("{w}x{h}"));
    }
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = kf.save_to_file(p);
}

pub fn get() -> Settings {
    CURRENT.with(|c| c.borrow().clone())
}

/// Reads one field without cloning the whole struct.
pub fn with<R>(f: impl FnOnce(&Settings) -> R) -> R {
    CURRENT.with(|c| f(&c.borrow()))
}

/// Mutates and persists.
pub fn update(f: impl FnOnce(&mut Settings)) {
    CURRENT.with(|c| {
        let mut s = c.borrow_mut();
        f(&mut s);
        save(&s);
    });
}
