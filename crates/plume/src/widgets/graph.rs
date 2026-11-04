//! Live, smoothly scrolling history graph.
//!
//! **GL path (default).** The widget hosts a `GtkGLArea`. History is reduced to
//! one value per device-pixel column (see `columns.rs`) and kept in a small
//! RGBA32F ring texture. Each frame the shader in `gl.rs` draws grid, fills and
//! lines in a single pass from that texture, positioned by one "scroll"
//! uniform. Consequences:
//!
//! * per sample (10 Hz): a few dozen columns are recomputed and uploaded
//!   (a few hundred bytes); nothing is rasterised on the CPU, nothing blocks;
//! * per frame: a handful of uniforms and one draw call;
//! * lines are exactly `line_px` device pixels wide at any sub-pixel scroll
//!   position, so motion is perfectly even and edges stay crisp;
//! * the auto-scaled y axis eases every frame instead of jumping per sample.
//!
//! **Fallback path.** Without OpenGL the curve is built as a GskPath per
//! sample, rendered into a texture and slid into place each frame.
//!
//! Either way the tick callback exists only while the graph is mapped, frames
//! are skipped when nothing moved, and unfocused windows (floating readouts,
//! background windows) run at a lower frame rate.

use super::columns::Columns;
use super::gl;
use crate::theme;
use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};
use plume_core::history::Series;
use plume_core::sampler::History;
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Cpu,
    CpuKernel,
    Core(usize),
    Mem,
    Swap,
    NetRx,
    NetTx,
    DiskRead,
    DiskWrite,
    DiskBusy,
    GpuBusy(usize),
    GpuMem(usize),
}

impl Key {
    pub fn series<'a>(&self, h: &'a History) -> Option<&'a Series> {
        match *self {
            Key::Cpu => Some(&h.cpu),
            Key::CpuKernel => Some(&h.cpu_kernel),
            Key::Core(i) => h.cores.get(i),
            Key::Mem => Some(&h.mem),
            Key::Swap => Some(&h.swap),
            Key::NetRx => Some(&h.net_rx),
            Key::NetTx => Some(&h.net_tx),
            Key::DiskRead => Some(&h.disk_read),
            Key::DiskWrite => Some(&h.disk_write),
            Key::DiskBusy => Some(&h.disk_busy),
            Key::GpuBusy(i) => h.gpu_busy.get(i),
            Key::GpuMem(i) => h.gpu_mem.get(i),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    Cpu,
    Kernel,
    Mem,
    Swap,
    Gpu,
    Rx,
    Tx,
    Read,
    Write,
    Core(usize),
}

impl Color {
    fn value(self, p: &theme::Palette) -> u32 {
        match self {
            Color::Cpu => p.cpu,
            Color::Kernel => p.kernel,
            Color::Mem => p.mem,
            Color::Swap => p.swap,
            Color::Gpu => p.gpu,
            Color::Rx => p.rx,
            Color::Tx => p.tx,
            Color::Read => p.read,
            Color::Write => p.write,
            Color::Core(i) => p.cores[i % p.cores.len()],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub key: Key,
    pub color: Color,
    pub fill: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    /// Values are fractions 0..1.
    Unit,
    /// Auto-ranging; `floor` is the minimum top of the axis.
    Auto { floor: f32 },
}

// Settings shared by all graphs.
thread_local! {
    /// Minimum µs between frames: focused window / everything else (0 = uncapped).
    static MIN_FRAME_FG: Cell<i64> = const { Cell::new(15_000) };
    static MIN_FRAME_BG: Cell<i64> = const { Cell::new(31_000) };
    static HISTORY_US: Cell<i64> = const { Cell::new(60_000_000) };
    static SMOOTH: Cell<bool> = const { Cell::new(false) };
    /// Line width in *device* pixels.
    static LINE_PX: Cell<f32> = const { Cell::new(1.5) };
    /// Bumped when any of the above changes, so graphs re-render.
    static CFG_GEN: Cell<u64> = const { Cell::new(1) };
}

fn bump() {
    CFG_GEN.with(|g| g.set(g.get() + 1));
}

fn cap_us(fps: u32) -> i64 {
    if fps == 0 { 0 } else { 1_000_000 / fps as i64 - 1_500 }
}

/// Frame-rate caps: for the focused window, and for unfocused windows and
/// floating readouts (which are almost always unfocused).
pub fn set_fps_caps(focused: u32, background: u32) {
    MIN_FRAME_FG.with(|c| c.set(cap_us(focused)));
    MIN_FRAME_BG.with(|c| c.set(cap_us(background)));
    bump();
}
pub fn set_history_secs(s: u32) {
    HISTORY_US.with(|c| c.set(s as i64 * 1_000_000));
    bump();
}
pub fn set_smooth(on: bool) {
    SMOOTH.with(|c| c.set(on));
    bump();
}
pub fn set_line_px(px: f32) {
    LINE_PX.with(|c| c.set(px.clamp(0.5, 6.0)));
    bump();
}

/// Which graph renderer is in use (for the About dialog's debug info).
pub fn renderer_name() -> &'static str {
    RENDERER.with(Cell::get)
}

/// Selectable time scales, in seconds.
pub const SCALES: [u32; 6] = [30, 60, 120, 300, 900, 3600];

pub fn scale_label(secs: u32) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s => format!("{} h", s / 3600),
    }
}

/// Next/previous entry in SCALES (dir > 0 = longer).
pub fn step_scale(cur: u32, dir: i32) -> u32 {
    let i = SCALES.iter().position(|&x| x >= cur).unwrap_or(SCALES.len() - 1) as i32;
    SCALES[(i + dir.signum()).clamp(0, SCALES.len() as i32 - 1) as usize]
}

/// Vertical grid spacing for a window: about six divisions, on round values.
fn grid_step_us(window: i64) -> i64 {
    const STEPS: [i64; 8] = [5, 10, 15, 30, 60, 120, 300, 600];
    let target = window / 6;
    STEPS.iter().map(|s| s * 1_000_000).find(|&s| s >= target).unwrap_or(600_000_000)
}

// ---------------------------------------------------------------------------
// Self-profiling (PLUME_PROFILE=1): time spent in our own graph code.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Prof {
    start: i64,
    ticks: u64,
    tick_us: i64,
    builds: u64,
    build_us: i64,
    draws: u64,
    draw_us: i64,
    upload_bytes: u64,
    /// Size changes of an already-visible graph (layout instability).
    resizes: u64,
}

thread_local! {
    static PROF: Cell<Prof> = Cell::new(Prof::default());
}

fn prof_add(f: impl FnOnce(&mut Prof)) {
    if !crate::profile::enabled() {
        return;
    }
    PROF.with(|p| {
        let now = glib::monotonic_time();
        let mut v = p.get();
        if v.start == 0 {
            v.start = now;
        }
        f(&mut v);
        let el = now - v.start;
        if el >= 5_000_000 {
            let s = el as f64 / 1e6;
            eprintln!(
                "plume-profile: {:.0} ticks/s ({:.1} µs), {:.1} data updates/s ({:.0} µs), {:.0} draws/s ({:.1} µs), \
                 {:.1} KiB/s uploaded, {} graph resizes → graph code = {:.2} % of one core",
                v.ticks as f64 / s,
                v.tick_us as f64 / v.ticks.max(1) as f64,
                v.builds as f64 / s,
                v.build_us as f64 / v.builds.max(1) as f64,
                v.draws as f64 / s,
                v.draw_us as f64 / v.draws.max(1) as f64,
                v.upload_bytes as f64 / 1024.0 / s,
                v.resizes,
                (v.tick_us + v.build_us + v.draw_us) as f64 / el as f64 * 100.0
            );
            v = Prof { start: now, ..Default::default() };
        }
        p.set(v);
    });
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Backend {
    #[default]
    Pending,
    Gl,
    Gsk,
}

#[derive(Default)]
struct GlRes {
    prog: Option<gl::Program>,
    tex: u32,
    vao: u32,
    tex_alloc: usize,
}

thread_local! {
    /// Programs are shared objects, so one link serves every GL area.
    static SHARED_PROG: Cell<Option<gl::Program>> = const { Cell::new(None) };
    static GL_WARNED: Cell<bool> = const { Cell::new(false) };
    static RENDERER: Cell<&'static str> = const { Cell::new("not started") };
    /// One GL context per window, shared by all of its graphs (each graph still
    /// renders into its own framebuffer). A context per GLArea costs several MB.
    static WINDOW_CTX: RefCell<Vec<(glib::WeakRef<gdk::Surface>, gdk::GLContext)>> = const { RefCell::new(Vec::new()) };
}

fn shared_context(area: &gtk::GLArea) -> Option<gdk::GLContext> {
    let surface = area.native()?.surface()?;
    WINDOW_CTX.with(|c| {
        let mut v = c.borrow_mut();
        v.retain(|(w, _)| w.upgrade().is_some());
        if let Some((_, ctx)) = v.iter().find(|(w, _)| w.upgrade().as_ref() == Some(&surface)) {
            return Some(ctx.clone());
        }
        let ctx = surface.create_gl_context().ok()?;
        ctx.set_allowed_apis(area.allowed_apis());
        ctx.realize().ok()?;
        v.push((surface.downgrade(), ctx.clone()));
        Some(ctx)
    })
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Graph {
        pub lines: RefCell<Vec<Line>>,
        pub scale: Cell<Option<Scale>>,
        pub grid: Cell<bool>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub on_ymax: RefCell<Option<Box<dyn Fn(f32)>>>,
        /// Per-graph time scale (floats); None = follow the global setting.
        pub window_override: Cell<Option<i64>>,
        pub delay: Cell<i64>,
        pub disp: Cell<i64>,
        pub last_frame: Cell<i64>,
        pub ymax: Cell<f32>,
        pub ymax_target: Cell<f32>,
        pub gens: RefCell<Vec<u64>>,
        pub last_size: Cell<(i32, i32)>,
        pub pts: RefCell<Vec<Vec<(i64, f32)>>>,
        backend: Cell<Backend>,

        // ---- GL path ----
        pub area: RefCell<Option<gtk::GLArea>>,
        cols: RefCell<Columns>,
        res: RefCell<GlRes>,
        /// Absolute column coordinate of the right edge for this frame.
        u0: Cell<f64>,
        last_u0: Cell<f64>,
        last_render: Cell<i64>,
        rendered_cfg: Cell<(u64, u64)>,
        fbo: Cell<(i32, i32)>,
        dev_scale: Cell<f64>,

        // ---- fallback (GSK texture) path ----
        tex: RefCell<Option<gdk::Texture>>,
        built_disp: Cell<i64>,
        built_size: Cell<(i32, i32)>,
        built_scale: Cell<f64>,
        built_theme: Cell<u64>,
        built_window: Cell<i64>,
        built_cfg: Cell<u64>,
        tex_w: Cell<f32>,
        last_rebuild: Cell<i64>,
        last_off_px: Cell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Graph {
        const NAME: &'static str = "PlumeGraph";
        type Type = super::Graph;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for Graph {
        fn constructed(&self) {
            self.parent_constructed();
            self.grid.set(true);
            self.ymax.set(1.0);
            self.ymax_target.set(1.0);
            self.obj().add_css_class("plume-graph");
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(a) = self.area.take() {
                a.unparent();
            }
        }
    }

    impl WidgetImpl for Graph {
        fn map(&self) {
            self.parent_map();
            let id = self.obj().add_tick_callback(|w, clock| {
                w.imp().on_tick(clock.frame_time());
                glib::ControlFlow::Continue
            });
            self.tick.replace(Some(id));
        }

        fn unmap(&self) {
            if let Some(id) = self.tick.take() {
                id.remove();
            }
            self.tex.replace(None);
            self.gens.borrow_mut().clear();
            self.parent_unmap();
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (40, 80, -1, -1),
                _ => (36, 120, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(a) = self.area.borrow().as_ref() {
                a.allocate(width, height, baseline, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            match self.backend.get() {
                Backend::Gsk => {
                    let t0 = glib::monotonic_time();
                    self.gsk_draw(snapshot);
                    prof_add(|p| {
                        p.draws += 1;
                        p.draw_us += glib::monotonic_time() - t0;
                    });
                }
                _ => {
                    if let Some(a) = self.area.borrow().as_ref() {
                        self.obj().snapshot_child(a, snapshot);
                    }
                }
            }
        }
    }

    impl Graph {
        pub(super) fn window_us(&self) -> i64 {
            self.window_override.get().unwrap_or_else(|| HISTORY_US.with(Cell::get))
        }

        // ------------------------------------------------------------------
        // Backend selection
        // ------------------------------------------------------------------

        pub(super) fn setup_gl(&self) {
            let area = gtk::GLArea::new();
            area.set_auto_render(false);
            area.set_has_depth_buffer(false);
            area.set_has_stencil_buffer(false);
            area.set_can_target(false);
            // PLUME_GL_API=gles|gl restricts the API (for testing both shader variants).
            match std::env::var("PLUME_GL_API").as_deref() {
                Ok("gles") => area.set_allowed_apis(gdk::GLAPI::GLES),
                Ok("gl") => area.set_allowed_apis(gdk::GLAPI::GL),
                _ => {}
            }
            area.set_parent(&*self.obj());
            area.connect_create_context(|a| {
                let ctx = shared_context(a);
                if ctx.is_none() {
                    a.set_error(Some(&glib::Error::new(gdk::GLError::NotAvailable, "no GL context")));
                }
                ctx
            });
            let weak = self.obj().downgrade();
            area.connect_realize(glib::clone!(
                #[strong]
                weak,
                move |a| {
                    if let Some(g) = weak.upgrade() {
                        g.imp().gl_realize(a);
                    }
                }
            ));
            area.connect_unrealize(glib::clone!(
                #[strong]
                weak,
                move |a| {
                    if let Some(g) = weak.upgrade() {
                        g.imp().gl_unrealize(a);
                    }
                }
            ));
            // Framebuffer size in device pixels. Tracked per area because with a
            // shared context the GL viewport is shared state (GTK only sets it
            // when an area is resized), so each render must set its own.
            area.connect_resize(glib::clone!(
                #[strong]
                weak,
                move |_, w, h| {
                    if let Some(g) = weak.upgrade() {
                        g.imp().fbo.set((w, h));
                    }
                }
            ));
            area.connect_render(move |a, _ctx| {
                if let Some(g) = weak.upgrade() {
                    g.imp().gl_render(a);
                }
                glib::Propagation::Stop
            });
            self.area.replace(Some(area));
        }

        pub(super) fn use_gsk(&self) {
            self.backend.set(Backend::Gsk);
            if let Some(a) = self.area.take() {
                a.unparent();
            }
            self.obj().queue_draw();
        }

        fn fallback(&self, why: &str) {
            RENDERER.with(|r| r.set("fallback (GskPath textures)"));
            if !GL_WARNED.with(|w| w.replace(true)) {
                eprintln!("plume: OpenGL graphs unavailable ({why}); using the fallback renderer");
            }
            // Can't unparent from inside the GLArea's own realize handler.
            let weak = self.obj().downgrade();
            glib::idle_add_local_once(move || {
                if let Some(g) = weak.upgrade() {
                    g.imp().use_gsk();
                }
            });
            self.backend.set(Backend::Gsk);
        }

        fn gl_realize(&self, a: &gtk::GLArea) {
            a.make_current();
            if let Some(e) = a.error() {
                return self.fallback(e.message());
            }
            let Some(g) = gl::api() else { return self.fallback("libepoxy not found") };
            let gles = a.api() == gdk::GLAPI::GLES;
            RENDERER.with(|r| r.set(if gles { "OpenGL ES (shader)" } else { "OpenGL (shader)" }));
            // Reuse the shared program if it is valid in this context's share group.
            let prog = match SHARED_PROG.with(Cell::get) {
                Some(p) if unsafe { (g.is_program())(p.id) } != 0 => p,
                _ => match gl::build_program(g, gles) {
                    Ok(p) => {
                        SHARED_PROG.with(|c| c.set(Some(p)));
                        p
                    }
                    Err(e) => return self.fallback(&format!("shader: {e}")),
                },
            };
            let mut res = self.res.borrow_mut();
            unsafe {
                (g.gen_vertex_arrays())(1, &mut res.vao);
                (g.gen_textures())(1, &mut res.tex);
                (g.bind_texture())(gl::TEXTURE_2D, res.tex);
                for (k, v) in [
                    (gl::TEXTURE_MIN_FILTER, gl::NEAREST),
                    (gl::TEXTURE_MAG_FILTER, gl::NEAREST),
                    (gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE),
                    (gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE),
                ] {
                    (g.tex_parameteri())(gl::TEXTURE_2D, k, v);
                }
            }
            res.prog = Some(prog);
            res.tex_alloc = 0;
            self.cols.borrow_mut().full = true;
            self.backend.set(Backend::Gl);
        }

        fn gl_unrealize(&self, a: &gtk::GLArea) {
            let Some(g) = gl::api() else { return };
            a.make_current();
            let mut res = self.res.borrow_mut();
            unsafe {
                if res.tex != 0 {
                    (g.delete_textures())(1, &res.tex);
                }
                if res.vao != 0 {
                    (g.delete_vertex_arrays())(1, &res.vao);
                }
            }
            *res = GlRes::default();
            self.cols.borrow_mut().texw = 0;
        }

        // ------------------------------------------------------------------
        // Per frame
        // ------------------------------------------------------------------

        fn on_tick(&self, frame_us: i64) {
            let focused = self.obj().root().and_downcast::<gtk::Window>().is_some_and(|w| w.is_active());
            let cap = if focused { MIN_FRAME_FG.with(Cell::get) } else { MIN_FRAME_BG.with(Cell::get) };
            let last = self.last_frame.get();
            if cap > 0 && frame_us - last < cap {
                return;
            }
            self.last_frame.set(frame_us);
            let dt_frame = (frame_us - last).clamp(0, 200_000);
            let t0 = glib::monotonic_time();
            let size = (self.obj().width(), self.obj().height());
            let prev = self.last_size.replace(size);
            if prev != (0, 0) && prev != size {
                prof_add(|p| p.resizes += 1);
            }
            match self.backend.get() {
                Backend::Gl => self.gl_tick(frame_us, dt_frame),
                Backend::Gsk => {
                    if let Some(b) = self.gsk_tick(frame_us) {
                        prof_add(|p| {
                            p.builds += 1;
                            p.build_us += b;
                        });
                    }
                }
                Backend::Pending => {}
            }
            prof_add(|p| {
                p.ticks += 1;
                p.tick_us += glib::monotonic_time() - t0;
            });
        }

        /// Reads the series' generations and sample interval; adapts the
        /// display delay. Returns (any data changed, interval).
        fn poll_data(&self) -> (bool, i64) {
            let mut changed = false;
            let mut interval = 0i64;
            crate::app::with_shared(|s| {
                let lines = self.lines.borrow();
                let mut gens = self.gens.borrow_mut();
                if gens.len() != lines.len() {
                    gens.clear();
                    gens.resize(lines.len(), u64::MAX);
                }
                for (l, g) in lines.iter().zip(gens.iter()) {
                    if let Some(series) = l.key.series(&s.hist) {
                        changed |= series.gen != *g;
                        interval = interval.max(series.interval_us().unwrap_or(0));
                    }
                }
            });
            // Run 1.5 sample intervals behind real time so a slightly late
            // sample never leaves the live edge empty. Hysteresis keeps it stable.
            if interval > 0 {
                let target = interval * 3 / 2 + 10_000;
                let d = self.delay.get();
                if d == 0 || (target - d).abs() > d / 4 {
                    self.delay.set(target);
                }
            }
            (changed, interval)
        }

        fn gl_tick(&self, frame_us: i64, dt_frame: i64) {
            let obj = self.obj();
            let (w, h) = (obj.width(), obj.height());
            if w <= 1 || h <= 1 {
                return;
            }
            let scale = obj.native().and_then(|n| n.surface()).map(|s| s.scale()).unwrap_or(1.0);
            // Use the real framebuffer size once known (GTK may round differently).
            let (fw, fh) = self.fbo.get();
            let est = ((w as f64 * scale).round() as i32, (h as f64 * scale).round() as i32);
            let wdev = if fw > 0 && (fw - est.0).abs() <= 1 && (fh - est.1).abs() <= 1 { fw } else { est.0 } as i64;
            self.dev_scale.set(scale);
            let window = self.window_us().max(1_000_000);
            let (data_changed, interval) = self.poll_data();
            let delay = self.delay.get();
            let disp = frame_us - delay;
            self.disp.set(disp);
            let dt = window as f64 / wdev as f64;
            let u0 = disp as f64 / dt;
            self.u0.set(u0);
            let right = u0.floor() as i64;
            let ahead = ((delay + interval.max(100_000) * 2) as f64 / dt).ceil() as i64 + 4;
            let texw = (wdev + ahead + 8) as usize;
            let n = self.lines.borrow().len().min(2);

            let mut cols = self.cols.borrow_mut();
            let structural = cols.texw != texw || (cols.dt - dt).abs() > 1e-9 || cols.n != n;
            if structural {
                cols.reset(texw, dt, n);
            }
            let mut updated = false;
            if structural || data_changed {
                let t0 = glib::monotonic_time();
                let c_lo = right - wdev - 2;
                let c_from = if structural || cols.final_until == i64::MIN { c_lo } else { cols.final_until.max(c_lo) };
                let c_to = right + ahead;
                let since = (c_from as f64 * dt) as i64 - interval.max(100_000) * 2 - dt as i64;
                let smooth = SMOOTH.with(Cell::get);
                let lines = self.lines.borrow();
                let mut pts = self.pts.borrow_mut();
                pts.resize_with(lines.len(), Vec::new);
                let mut newest = i64::MAX;
                crate::app::with_shared(|s| {
                    let mut gens = self.gens.borrow_mut();
                    for (i, l) in lines.iter().enumerate().take(4) {
                        match l.key.series(&s.hist) {
                            Some(series) => {
                                series.copy_since(since, &mut pts[i]);
                                gens[i] = series.gen;
                                newest = newest.min(series.last().map_or(i64::MIN, |p| p.0));
                            }
                            None => pts[i].clear(),
                        }
                    }
                });
                for (i, p) in pts.iter().enumerate().take(n) {
                    cols.update_series(i, p, c_from, c_to, smooth);
                }
                cols.update_env(c_from, c_to);
                if newest != i64::MAX && newest != i64::MIN {
                    cols.final_until = (newest as f64 / dt - 0.5).floor() as i64;
                }
                // Y range target from what is (about to be) visible.
                if let Some(Scale::Auto { floor }) = self.scale.get() {
                    let peak = cols.max_in(right - wdev, right + 1);
                    let target = crate::format::nice_ceiling(peak.max(floor) * 1.08);
                    if target != self.ymax_target.get() {
                        self.ymax_target.set(target);
                        if let Some(cb) = self.on_ymax.borrow().as_ref() {
                            cb(target);
                        }
                    }
                    if structural {
                        self.ymax.set(target);
                    }
                }
                updated = true;
                prof_add(|p| {
                    p.builds += 1;
                    p.build_us += glib::monotonic_time() - t0;
                });
            }
            drop(cols);

            // Ease the y axis: quickly up (avoid clipping), gently down.
            let (cur, tgt) = (self.ymax.get(), self.ymax_target.get());
            let mut easing = false;
            if self.scale.get() != Some(Scale::Unit) && (cur - tgt).abs() > tgt * 0.002 {
                let tau = if tgt > cur { 90_000.0 } else { 450_000.0 };
                let k = 1.0 - (-(dt_frame.max(1) as f64) / tau).exp();
                self.ymax.set(cur + (tgt - cur) * k as f32);
                easing = true;
            } else if self.scale.get() == Some(Scale::Unit) {
                self.ymax.set(1.0);
            }

            // Render only if something visibly changed.
            let cfg = (theme::generation(), CFG_GEN.with(Cell::get));
            let moved = (u0 - self.last_u0.get()).abs();
            let since_render = frame_us - self.last_render.get();
            let need = structural
                || easing
                || cfg != self.rendered_cfg.get()
                || moved >= 0.25
                || (updated && (moved >= 0.05 || since_render >= 250_000));
            if need {
                if let Some(a) = self.area.borrow().as_ref() {
                    a.queue_render();
                }
                self.last_render.set(frame_us);
            }
        }

        fn gl_render(&self, _a: &gtk::GLArea) {
            let Some(g) = gl::api() else { return };
            let t0 = glib::monotonic_time();
            let res = self.res.borrow();
            let Some(prog) = res.prog else { return };
            let (mut fw, mut fh) = self.fbo.get();
            if fw <= 0 || fh <= 0 {
                let mut vp = [0i32; 4];
                unsafe { (g.get_integerv())(gl::VIEWPORT, vp.as_mut_ptr()) };
                (fw, fh) = (vp[2].max(1), vp[3].max(1));
                self.fbo.set((fw, fh));
            }
            unsafe { (g.viewport())(0, 0, fw, fh) };

            let mut cols = self.cols.borrow_mut();
            if cols.texw == 0 {
                return;
            }
            let mut bytes = 0u64;
            unsafe {
                (g.active_texture())(gl::TEXTURE0);
                (g.bind_texture())(gl::TEXTURE_2D, res.tex);
                (g.pixel_storei())(gl::UNPACK_ALIGNMENT, 4);
                if cols.full || res.tex_alloc != cols.texw {
                    // Row 0: values; row 1: envelopes.
                    (g.tex_image_2d())(
                        gl::TEXTURE_2D,
                        0,
                        gl::RGBA32F,
                        cols.texw as i32,
                        2,
                        0,
                        gl::RGBA,
                        gl::FLOAT,
                        std::ptr::null(),
                    );
                    for (row, buf) in [(0, &cols.data), (1, &cols.env)] {
                        (g.tex_sub_image_2d())(
                            gl::TEXTURE_2D,
                            0,
                            0,
                            row,
                            cols.texw as i32,
                            1,
                            gl::RGBA,
                            gl::FLOAT,
                            buf.as_ptr() as *const _,
                        );
                    }
                    bytes += (cols.texw * 32) as u64;
                    cols.full = false;
                    cols.dirty = None;
                    drop(res);
                    self.res.borrow_mut().tex_alloc = cols.texw;
                } else {
                    drop(res);
                    for (slot, len) in cols.take_dirty() {
                        for (row, buf) in [(0, &cols.data), (1, &cols.env)] {
                            (g.tex_sub_image_2d())(
                                gl::TEXTURE_2D,
                                0,
                                slot as i32,
                                row,
                                len as i32,
                                1,
                                gl::RGBA,
                                gl::FLOAT,
                                buf[slot..slot + len].as_ptr() as *const _,
                            );
                        }
                        bytes += (len * 32) as u64;
                    }
                }
            }
            let texw = cols.texw;
            let n = cols.n;
            drop(cols);

            // Uniforms.
            let p = theme::palette();
            let lines = self.lines.borrow();
            let mut fill = [0f32; 8];
            let mut line = [0f32; 8];
            for (i, l) in lines.iter().enumerate().take(2) {
                let c = theme::rgba(l.color.value(&p), 1.0);
                let fa = p.fill_alpha();
                if l.fill {
                    fill[i * 4..i * 4 + 4].copy_from_slice(&[c.red() * fa, c.green() * fa, c.blue() * fa, fa]);
                }
                line[i * 4..i * 4 + 4].copy_from_slice(&[c.red(), c.green(), c.blue(), 1.0]);
            }
            let grid = if self.grid.get() {
                let gc = theme::rgba(p.fg, 1.0);
                let ga = p.grid_alpha();
                [gc.red() * ga, gc.green() * ga, gc.blue() * ga, ga]
            } else {
                [0.0; 4]
            };
            // Vertical time grid: positions in device px from the left edge.
            let window = self.window_us().max(1);
            let dt = window as f64 / fw as f64;
            let disp = self.disp.get();
            let step = grid_step_us(window);
            let mut vgrid = [0f32; 8];
            let mut nv = 0;
            let mut t = disp - disp.rem_euclid(step);
            while t > disp - window && nv < 8 {
                vgrid[nv] = (fw as f64 - (disp - t) as f64 / dt) as f32;
                nv += 1;
                t -= step;
            }
            let u0 = self.u0.get();
            let scale = self.dev_scale.get().max(1.0) as f32;
            let res = self.res.borrow();
            unsafe {
                (g.disable())(gl::BLEND);
                (g.disable())(gl::DEPTH_TEST);
                (g.disable())(gl::SCISSOR_TEST);
                (g.use_program())(prog.id);
                let l = prog.loc;
                (g.uniform1i())(l.data, 0);
                (g.uniform1i())(l.texw, texw as i32);
                (g.uniform1f())(l.u0, u0.rem_euclid(texw as f64) as f32);
                (g.uniform2f())(l.size, fw as f32, fh as f32);
                (g.uniform1f())(l.ymax, self.ymax.get().max(1e-6));
                (g.uniform1f())(l.pad, 2.0 * scale);
                (g.uniform1i())(l.n, n as i32);
                (g.uniform4fv())(l.fill, 2, fill.as_ptr());
                (g.uniform4fv())(l.line, 2, line.as_ptr());
                (g.uniform1f())(l.hw, LINE_PX.with(Cell::get) * 0.5);
                (g.uniform4fv())(l.grid, 1, grid.as_ptr());
                (g.uniform1fv())(l.vgrid, 8, vgrid.as_ptr());
                (g.uniform1i())(l.nv, nv as i32);
                (g.bind_vertex_array())(res.vao);
                (g.draw_arrays())(gl::TRIANGLES, 0, 3);
                (g.bind_vertex_array())(0);
                (g.use_program())(0);
            }
            self.last_u0.set(u0);
            self.rendered_cfg.set((theme::generation(), CFG_GEN.with(Cell::get)));
            prof_add(|pr| {
                pr.draws += 1;
                pr.draw_us += glib::monotonic_time() - t0;
                pr.upload_bytes += bytes;
            });
        }

        // ------------------------------------------------------------------
        // Fallback renderer (no OpenGL): GskPath → texture per sample.
        // ------------------------------------------------------------------

        fn gsk_draw(&self, snapshot: &gtk::Snapshot) {
            let w = self.obj().width() as f32;
            let h = self.obj().height() as f32;
            if w <= 1.0 || h <= 1.0 {
                return;
            }
            let p = theme::palette();
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, w, h));
            if self.grid.get() {
                let grid = theme::rgba(p.fg, p.grid_alpha());
                for i in 1..4 {
                    let y = (h * i as f32 / 4.0).round();
                    snapshot.append_color(&grid, &graphene::Rect::new(0.0, y, w, 1.0));
                }
                let window = self.window_us().max(1);
                let disp = self.disp.get();
                let pps = w as f64 / window as f64;
                let step = grid_step_us(window);
                let mut t = disp - disp.rem_euclid(step);
                while t > disp - window {
                    let x = (w as f64 - (disp - t) as f64 * pps) as f32;
                    snapshot.append_color(&grid, &graphene::Rect::new(x.round(), 0.0, 1.0, h));
                    t -= step;
                }
            }
            if let Some(tex) = self.tex.borrow().as_ref() {
                let window = self.built_window.get().max(1);
                let pps = w as f64 / window as f64;
                let off = ((self.disp.get() - self.built_disp.get()) as f64 * pps) as f32;
                let tw = self.tex_w.get();
                let off = off.clamp(0.0, (tw - w).max(0.0));
                let s = self.built_scale.get().max(1.0) as f32;
                let off = (off * s).round() / s;
                let (rw, rh) = (tex.width() as f32 / s, tex.height() as f32 / s);
                let filter = if s.fract() == 0.0 { gsk::ScalingFilter::Nearest } else { gsk::ScalingFilter::Linear };
                snapshot.append_scaled_texture(tex, filter, &graphene::Rect::new(-off, 0.0, rw, rh));
            }
            snapshot.pop();
        }

        /// Returns the rebuild time in µs if a rebuild happened.
        fn gsk_tick(&self, frame_us: i64) -> Option<i64> {
            let obj = self.obj();
            let size = (obj.width(), obj.height());
            if size.0 <= 1 || size.1 <= 1 {
                return None;
            }
            let scale = obj.native().and_then(|n| n.surface()).map(|s| s.scale()).unwrap_or(1.0);
            let window = self.window_us();
            let cfg = CFG_GEN.with(Cell::get);
            let structural = size != self.built_size.get()
                || scale != self.built_scale.get()
                || theme::generation() != self.built_theme.get()
                || window != self.built_window.get()
                || cfg != self.built_cfg.get();
            let (new_data, interval) = self.poll_data();
            let disp = frame_us - self.delay.get();
            self.disp.set(disp);
            let tw = self.tex_w.get();
            let pps = size.0 as f64 / window.max(1) as f64;
            let off = (disp - self.built_disp.get()) as f64 * pps;
            let mut changed = structural;
            if new_data {
                let since = frame_us - self.last_rebuild.get();
                if off * scale >= 1.0 || since >= 1_000_000 || window <= 120_000_000 {
                    changed = true;
                }
            }
            if changed || self.tex.borrow().is_none() || off as f32 > (tw - size.0 as f32) * 0.8 {
                let t0 = glib::monotonic_time();
                self.gsk_rebuild(size, scale, window, disp, interval);
                self.last_rebuild.set(frame_us);
                obj.queue_draw();
                return Some(glib::monotonic_time() - t0);
            }
            let off_px = ((disp - self.built_disp.get()) as f64 * pps * scale).round() as i64;
            if off_px != self.last_off_px.get() {
                self.last_off_px.set(off_px);
                obj.queue_draw();
            }
            None
        }

        fn gsk_rebuild(&self, (w, h): (i32, i32), scale: f64, window: i64, disp: i64, interval: i64) {
            let obj = self.obj();
            let Some(renderer) = obj.native().and_then(|n| n.renderer()) else { return };
            let (wf, hf) = (w as f32, h as f32);
            let pps = wf as f64 / window.max(1) as f64;
            let margin = ((interval.max(50_000) as f64 * 2.5 + 150_000.0) * pps).ceil() as f32 + 4.0;
            let tw = wf + margin;
            let since = disp - window - interval.max(100_000) * 2;
            let lines = self.lines.borrow().clone();
            let mut pts = self.pts.borrow_mut();
            pts.resize_with(lines.len(), Vec::new);
            crate::app::with_shared(|s| {
                let mut gens = self.gens.borrow_mut();
                for (i, l) in lines.iter().enumerate() {
                    match l.key.series(&s.hist) {
                        Some(series) => {
                            series.copy_since(since, &mut pts[i]);
                            if let Some(g) = gens.get_mut(i) {
                                *g = series.gen;
                            }
                        }
                        None => pts[i].clear(),
                    }
                }
            });
            for buf in pts.iter_mut() {
                decimate(buf, pps, disp, wf as f64);
            }
            let ymax = match self.scale.get().unwrap_or(Scale::Unit) {
                Scale::Unit => 1.0,
                Scale::Auto { floor } => {
                    let peak = pts
                        .iter()
                        .flat_map(|v| v.iter())
                        .filter(|(t, _)| *t >= disp - window)
                        .map(|(_, v)| *v)
                        .fold(0.0f32, f32::max);
                    let target = crate::format::nice_ceiling(peak.max(floor) * 1.08);
                    let cur = self.ymax.get();
                    let next = if target >= cur { target } else { cur + (target - cur) * 0.15 };
                    if let Some(cb) = self.on_ymax.borrow().as_ref() {
                        cb(next);
                    }
                    next
                }
            };
            self.ymax.set(ymax);
            let p = theme::palette();
            let s = scale as f32;
            let snap = gtk::Snapshot::new();
            let smooth = SMOOTH.with(Cell::get);
            let top_pad = 2.0f32;
            for pass in [true, false] {
                for (l, buf) in lines.iter().zip(pts.iter()) {
                    if buf.len() < 2 || (pass && !l.fill) {
                        continue;
                    }
                    let xy = |&(t, v): &(i64, f32)| -> (f32, f32) {
                        let x = (wf as f64 - (disp - t) as f64 * pps) as f32;
                        let y = hf - (v / ymax).clamp(0.0, 1.0) * (hf - top_pad);
                        (x * s, y * s)
                    };
                    let b = gsk::PathBuilder::new();
                    let (x0, y0) = xy(&buf[0]);
                    if pass {
                        b.move_to(x0, hf * s);
                        b.line_to(x0, y0);
                    } else {
                        b.move_to(x0, y0);
                    }
                    let mut prev = (x0, y0);
                    if smooth {
                        let p: Vec<(f32, f32)> = buf.iter().map(xy).collect();
                        for i in 0..p.len() - 1 {
                            let p0 = p[i.saturating_sub(1)];
                            let (p1, p2) = (p[i], p[i + 1]);
                            let p3 = p[(i + 2).min(p.len() - 1)];
                            let (lo, hi) = (p1.1.min(p2.1), p1.1.max(p2.1));
                            let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, (p1.1 + (p2.1 - p0.1) / 6.0).clamp(lo, hi));
                            let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, (p2.1 - (p3.1 - p1.1) / 6.0).clamp(lo, hi));
                            b.cubic_to(c1.0, c1.1, c2.0, c2.1, p2.0, p2.1);
                        }
                        prev = *p.last().unwrap();
                    } else {
                        for pt in &buf[1..] {
                            let (x, y) = xy(pt);
                            b.line_to(x, y);
                            prev = (x, y);
                        }
                    }
                    let color = l.color.value(&p);
                    if pass {
                        b.line_to(prev.0, hf * s);
                        b.close();
                        snap.append_fill(&b.to_path(), gsk::FillRule::Winding, &theme::rgba(color, p.fill_alpha()));
                    } else {
                        let stroke = gsk::Stroke::new(LINE_PX.with(Cell::get));
                        stroke.set_line_join(gsk::LineJoin::Miter);
                        stroke.set_miter_limit(5.0);
                        snap.append_stroke(&b.to_path(), &stroke, &theme::rgba(color, 1.0));
                    }
                }
            }
            drop(pts);
            let viewport = graphene::Rect::new(0.0, 0.0, (tw * s).ceil(), (hf * s).ceil());
            self.tex.replace(snap.to_node().map(|node| renderer.render_texture(&node, Some(&viewport))));
            self.built_size.set((w, h));
            self.built_scale.set(scale);
            self.built_theme.set(theme::generation());
            self.built_window.set(window);
            self.built_cfg.set(CFG_GEN.with(Cell::get));
            self.built_disp.set(disp);
            self.tex_w.set(tw);
        }
    }
}

/// Collapses runs of points that land in the same pixel column into their
/// maximum (area charts must not hide peaks). No-op when sparse.
fn decimate(buf: &mut Vec<(i64, f32)>, pps: f64, disp: i64, width: f64) {
    if buf.len() < 4 || (buf.len() as f64) < width * 1.5 {
        return;
    }
    let col = |t: i64| ((disp - t) as f64 * pps).floor() as i64;
    let mut out = 0usize;
    let mut i = 0usize;
    while i < buf.len() {
        let c = col(buf[i].0);
        let mut best = buf[i];
        let mut j = i + 1;
        while j < buf.len() && col(buf[j].0) == c {
            if buf[j].1 > best.1 {
                best.1 = buf[j].1;
            }
            best.0 = buf[j].0;
            j += 1;
        }
        buf[out] = best;
        out += 1;
        i = j;
    }
    buf.truncate(out);
}

#[cfg(test)]
mod tests {
    #[test]
    fn scales_step_and_grid() {
        assert_eq!(super::step_scale(60, 1), 120);
        assert_eq!(super::step_scale(60, -1), 30);
        assert_eq!(super::step_scale(30, -1), 30);
        assert_eq!(super::step_scale(3600, 1), 3600);
        assert_eq!(super::grid_step_us(60_000_000), 10_000_000);
        assert_eq!(super::grid_step_us(3600_000_000), 600_000_000);
        assert_eq!(super::scale_label(900), "15 min");
    }

    #[test]
    fn decimate_keeps_peaks() {
        let mut v: Vec<(i64, f32)> = (0..1000).map(|i| (i as i64 * 1000, if i == 500 { 9.0 } else { 1.0 })).collect();
        super::decimate(&mut v, 100.0 / 1_000_000.0, 1_000_000, 100.0);
        assert!(v.len() <= 102, "{}", v.len());
        assert!(v.iter().any(|p| p.1 == 9.0));
    }
}

glib::wrapper! {
    pub struct Graph(ObjectSubclass<imp::Graph>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Graph {
    pub fn new(lines: &[Line], scale: Scale) -> Graph {
        let g: Graph = glib::Object::new();
        g.imp().lines.replace(lines.to_vec());
        g.imp().scale.set(Some(scale));
        g.set_hexpand(true);
        g.set_vexpand(true);
        g.update_property(&[gtk::accessible::Property::Label("Usage history graph")]);
        let force_gsk = std::env::var("PLUME_GRAPH").is_ok_and(|v| v == "gsk");
        // The shader handles up to two series per graph (all of Plume's graphs).
        if force_gsk || gl::api().is_none() || lines.len() > 2 {
            RENDERER.with(|r| r.set("fallback (GskPath textures)"));
            g.imp().use_gsk();
        } else {
            g.imp().setup_gl();
        }
        g
    }

    pub fn set_grid(&self, on: bool) {
        self.imp().grid.set(on);
    }

    /// Gives this graph its own time scale (None = follow the global one).
    pub fn set_window_secs(&self, secs: Option<u32>) {
        self.imp().window_override.set(secs.map(|s| s as i64 * 1_000_000));
    }

    /// Calls `f(+1 | -1)` when the user scrolls over the graph (longer /
    /// shorter time scale). With `need_ctrl`, only Ctrl+scroll counts, so
    /// plain scrolling still scrolls the page.
    pub fn connect_zoom(&self, need_ctrl: bool, f: impl Fn(i32) + 'static) {
        let c = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE);
        c.connect_scroll(move |c, _dx, dy| {
            let ctrl = c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            if need_ctrl && !ctrl {
                return glib::Propagation::Proceed;
            }
            if dy != 0.0 {
                f(if dy > 0.0 { 1 } else { -1 });
            }
            glib::Propagation::Stop
        });
        self.add_controller(c);
        self.set_tooltip_text(Some(if need_ctrl { "Ctrl+scroll to change the time scale" } else { "Scroll to change the time scale" }));
    }

    /// Called with the axis maximum whenever an auto-scaled graph's target changes.
    pub fn connect_ymax(&self, f: impl Fn(f32) + 'static) {
        self.imp().on_ymax.replace(Some(Box::new(f)));
    }
}
