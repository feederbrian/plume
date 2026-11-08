//! Renderer measurements on the user's own machine.
//!
//! `plume --bench[=SECS]` shows the Overview, runs each rendering variant for
//! SECS seconds (default 15) after a short warm-up, from the 0.2.1 renderer to
//! the current defaults, prints a table and quits. For each variant it reports
//! the GPU time of Plume's own DRM clients and of the compositor (from the
//! kernel's per-client accounting in /proc/PID/fdinfo, as intel_gpu_top uses),
//! how often the window redrew, how many graph draws that took, and Plume's
//! CPU time.
//!
//! With PLUME_PROFILE=1 the same GPU numbers are printed every 5 s, and the
//! `app.debug-render` action switches variants live ("full", "strip", "cull",
//! "nocull", "step:PX", "freeze", "thaw"), e.g. for comparing screenshots.

use crate::widgets::graph::{self, RenderOpts};
use crate::{app, settings};
use adw::prelude::*;
use gtk::{gio, glib};
use plume_core::gpu::drm::{self, EngineCounters};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const WARMUP_US: i64 = 3_000_000;

struct Variant {
    name: &'static str,
    opts: RenderOpts,
    fps: Option<u32>,
}

fn variants() -> Vec<Variant> {
    let old = RenderOpts { strip: false, cull: false, step_px: 0.25 };
    let new = RenderOpts::DEFAULT;
    vec![
        Variant { name: "0.2.1 renderer", opts: old, fps: None },
        Variant { name: "+ skip off-screen graphs", opts: RenderOpts { cull: true, ..old }, fps: None },
        Variant { name: "+ shade under the curves only", opts: RenderOpts { strip: true, ..RenderOpts { cull: true, ..old } }, fps: None },
        Variant { name: "+ 1 px paced redraws (default)", opts: new, fps: None },
        Variant { name: "default, 30 fps cap", opts: new, fps: Some(30) },
    ]
}

/// The compositor process (same user), for its share of the GPU.
fn compositor() -> Option<(i32, String)> {
    use std::os::unix::fs::MetadataExt;
    let uid = unsafe { libc::getuid() };
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else { continue };
        let comm = comm.trim();
        let known = matches!(comm, "gnome-shell" | "kwin_wayland" | "sway" | "Hyprland" | "weston" | "cosmic-comp" | "niri");
        if known && e.metadata().map(|m| m.uid() == uid).unwrap_or(false) {
            return Some((pid, comm.to_string()));
        }
    }
    None
}

fn cpu_time_us() -> i64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    (ru.ru_utime.tv_sec + ru.ru_stime.tv_sec) * 1_000_000 + (ru.ru_utime.tv_usec + ru.ru_stime.tv_usec)
}

/// One measurement point.
struct Snap {
    t_us: i64,
    cpu_us: i64,
    me: EngineCounters,
    comp: Option<EngineCounters>,
    paints: u64,
    draws: u64,
}

fn snap(comp_pid: Option<i32>, paints: u64) -> Snap {
    Snap {
        t_us: glib::monotonic_time(),
        cpu_us: cpu_time_us(),
        me: drm::process_counters(std::process::id() as i32),
        comp: comp_pid.map(drm::process_counters),
        paints,
        draws: graph::draw_count(),
    }
}

struct Row {
    name: String,
    gpu_me: Option<f32>,
    gpu_comp: Option<f32>,
    fps: f64,
    draws: f64,
    cpu: f64,
}

fn row(name: &str, a: &Snap, b: &Snap) -> Row {
    let dt = (b.t_us - a.t_us).max(1);
    let gpu = |x: &EngineCounters, y: &EngineCounters| drm::main_engine_busy(x, y, dt as u64 * 1000).map(|(_, f)| f * 100.0);
    Row {
        name: name.to_string(),
        gpu_me: gpu(&a.me, &b.me),
        gpu_comp: match (&a.comp, &b.comp) {
            (Some(x), Some(y)) => gpu(x, y),
            _ => None,
        },
        fps: (b.paints - a.paints) as f64 * 1e6 / dt as f64,
        draws: (b.draws - a.draws) as f64 * 1e6 / dt as f64,
        cpu: (b.cpu_us - a.cpu_us) as f64 / dt as f64 * 100.0,
    }
}

fn pct(v: Option<f32>) -> String {
    v.map_or_else(|| "n/a".into(), |v| format!("{v:.1} %"))
}

fn print_header(comp: &Option<(i32, String)>) {
    let cname = comp.as_ref().map_or("compositor", |c| c.1.as_str());
    println!(
        "{:<34} {:>10} {:>14} {:>10} {:>11} {:>12} {:>10}",
        "variant", "Plume GPU", format!("{cname} GPU"), "GPU total", "window fps", "graph draws", "Plume CPU"
    );
}

fn print_row(r: &Row) {
    let total = match (r.gpu_me, r.gpu_comp) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
    println!(
        "{:<34} {:>10} {:>14} {:>10} {:>11.1} {:>10.0}/s {:>8.1} %",
        r.name,
        pct(r.gpu_me),
        pct(r.gpu_comp),
        pct(total),
        r.fps,
        r.draws,
        r.cpu
    );
}

/// Counts frames in which the window actually painted.
fn count_paints(win: &gtk::Window, counter: Rc<Cell<u64>>) {
    let hook = move |w: &gtk::Window| {
        if let Some(clock) = w.frame_clock() {
            let c = counter.clone();
            clock.connect_paint(move |_| c.set(c.get() + 1));
        }
    };
    if win.is_realized() {
        hook(win);
    } else {
        win.connect_realize(move |w| hook(w));
    }
}

#[derive(PartialEq)]
enum Phase {
    Settle,
    Warm,
    Measure,
}

pub fn start(secs: u32) {
    let secs = secs.clamp(3, 600) as i64;
    let page = settings::with(|s| s.page.clone());
    app::show_main();
    app::show_tab("overview");
    let Some(win) = app::main_window() else {
        eprintln!("plume-bench: no main window");
        return;
    };
    let paints = Rc::new(Cell::new(0u64));
    count_paints(win.upcast_ref(), paints.clone());
    let user = graph::render_opts();
    let comp = compositor();
    let vs = variants();
    let t0 = glib::monotonic_time();
    let state = Rc::new(RefCell::new((Phase::Settle, t0, 0usize, None::<Snap>, Vec::<Row>::new())));
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let now = glib::monotonic_time();
        let mut st = state.borrow_mut();
        let (phase, since, i, a, rows) = &mut *st;
        let apply = |v: &Variant| {
            graph::set_render_opts(v.opts);
            let (fg, bg) = settings::with(|s| (s.fps_cap, s.fps_cap_bg));
            let f = v.fps.unwrap_or(fg);
            graph::set_fps_caps(f, v.fps.unwrap_or(bg));
        };
        match phase {
            Phase::Settle if now - *since >= 4_000_000 => {
                let w = &win;
                let scale = w.surface().map_or(1.0, |s| s.scale());
                let refresh = w.frame_clock().map_or(0, |c| c.refresh_info(c.frame_time()).0);
                println!(
                    "plume-bench {}: {}×{} px window (scale {scale}), {:.0} Hz, GTK renderer {}, graphs: {}",
                    env!("CARGO_PKG_VERSION"),
                    (w.width() as f64 * scale).round(),
                    (w.height() as f64 * scale).round(),
                    if refresh > 0 { 1e6 / refresh as f64 } else { 0.0 },
                    app::gsk_renderer_name(),
                    graph::renderer_name(),
                );
                println!("plume-bench: {secs} s per variant after a 3 s warm-up; keep the window visible and the machine idle.");
                if comp.is_none() {
                    println!("plume-bench: compositor not found (or not readable); its GPU share is not included.");
                }
                print_header(&comp);
                apply(&vs[0]);
                (*phase, *since) = (Phase::Warm, now);
            }
            Phase::Warm if now - *since >= WARMUP_US => {
                *a = Some(snap(comp.as_ref().map(|c| c.0), paints.get()));
                (*phase, *since) = (Phase::Measure, now);
            }
            Phase::Measure if now - *since >= secs * 1_000_000 => {
                let b = snap(comp.as_ref().map(|c| c.0), paints.get());
                let r = row(vs[*i].name, a.as_ref().unwrap_or(&b), &b);
                print_row(&r);
                rows.push(r);
                *i += 1;
                if *i < vs.len() {
                    apply(&vs[*i]);
                    (*phase, *since) = (Phase::Warm, now);
                } else {
                    summary(rows);
                    // Leave the user's settings as they were (showing the Overview saved it).
                    settings::update(|s| s.page = page.clone());
                    graph::set_render_opts(user);
                    let (fg, bg) = settings::with(|s| (s.fps_cap, s.fps_cap_bg));
                    graph::set_fps_caps(fg, bg);
                    if let Some(app) = app::application() {
                        app.quit();
                    }
                    return glib::ControlFlow::Break;
                }
            }
            _ => {}
        }
        glib::ControlFlow::Continue
    });
}

fn summary(rows: &[Row]) {
    let (Some(first), Some(def)) = (rows.first(), rows.iter().find(|r| r.name.contains("(default)"))) else { return };
    let total = |r: &Row| match (r.gpu_me, r.gpu_comp) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
    match (total(first), total(def)) {
        (Some(a), Some(b)) if a > 0.0 => println!(
            "plume-bench: GPU time, default vs 0.2.1 renderer: {:.1} % → {:.1} % ({:+.0} %)",
            a,
            b,
            (b - a) / a * 100.0
        ),
        _ => println!(
            "plume-bench: no GPU accounting here (software rendering?). CPU, default vs 0.2.1: {:.1} % → {:.1} %",
            first.cpu, def.cpu
        ),
    }
}

/// PLUME_PROFILE=1: GPU busy of Plume and the compositor every 5 s.
pub fn start_gpu_report() {
    let comp = compositor();
    let pid = std::process::id() as i32;
    let prev = RefCell::new((glib::monotonic_time(), drm::process_counters(pid), comp.as_ref().map(|c| drm::process_counters(c.0))));
    glib::timeout_add_local(std::time::Duration::from_secs(5), move || {
        let now = glib::monotonic_time();
        let me = drm::process_counters(pid);
        let cc = comp.as_ref().map(|c| drm::process_counters(c.0));
        let fmt = |x: Option<(String, f32)>| x.map_or_else(|| "n/a".into(), |(e, f)| format!("{e} {:.1} %", f * 100.0));
        let line = {
            let (t, me0, cc0) = &*prev.borrow();
            let dt = ((now - t).max(1) as u64) * 1000;
            let comp_part = match (&comp, cc0, &cc) {
                (Some((_, name)), Some(a), Some(b)) => format!(" · {name}: {}", fmt(drm::main_engine_busy(a, b, dt))),
                _ => String::new(),
            };
            format!("plume-gpu: Plume: {}{comp_part} (engine busy, last 5 s)", fmt(drm::main_engine_busy(me0, &me, dt)))
        };
        eprintln!("{line}");
        prev.replace((now, me, cc));
        glib::ControlFlow::Continue
    });
}

/// PLUME_PROFILE=1: `app.debug-render` switches renderer variants live.
pub fn register_debug_action(app: &adw::Application) {
    let a = gio::SimpleAction::new("debug-render", Some(glib::VariantTy::STRING));
    a.connect_activate(|_, p| {
        let Some(v) = p.and_then(|v| v.get::<String>()) else { return };
        let o = graph::render_opts();
        match v.as_str() {
            "full" => graph::set_render_opts(RenderOpts { strip: false, ..o }),
            "strip" => graph::set_render_opts(RenderOpts { strip: true, ..o }),
            "cull" => graph::set_render_opts(RenderOpts { cull: true, ..o }),
            "nocull" => graph::set_render_opts(RenderOpts { cull: false, ..o }),
            "freeze" => graph::freeze_at(Some(glib::monotonic_time() - 1_000_000)),
            "thaw" => graph::freeze_at(None),
            s => match s.strip_prefix("step:").and_then(|x| x.parse::<f32>().ok()) {
                Some(px) => graph::set_motion_step(px),
                None => eprintln!("plume: debug-render: unknown variant {s}"),
            },
        }
        eprintln!("plume: render options now {:?}", graph::render_opts());
    });
    app.add_action(&a);
}
