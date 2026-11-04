//! Live, smoothly scrolling history graph.
//!
//! Drawing cost is the reason this widget exists. A naive approach rebuilds
//! and rasterises the curve every frame (which is what made GNOME System
//! Monitor's "smooth" graphs expensive). Instead:
//!
//! 1. When a **new sample** arrives (~10 Hz), the curve is built once as a
//!    GskPath and rendered **on the GPU into a texture** that is slightly wider
//!    than the widget.
//! 2. Every **frame** (60 Hz), the widget only emits a clip, a handful of grid
//!    rectangles and that texture shifted left by a sub-pixel offset. That is a
//!    GPU blit, so the CPU cost per frame is close to zero.
//! 3. The display runs one sample interval behind real time, so there is
//!    always real data at the right edge and the scroll never stutters.
//!
//! The tick callback is installed only while the widget is mapped, and GTK
//! stops ticking for hidden, minimised or suspended windows, so an invisible
//! graph costs nothing.

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

// Frame-rate cap shared by all graphs (µs between frames; 0 = uncapped).
thread_local! {
    static MIN_FRAME_US: Cell<i64> = const { Cell::new(16_000) };
    static HISTORY_US: Cell<i64> = const { Cell::new(60_000_000) };
    static SMOOTH: Cell<bool> = const { Cell::new(true) };
}

// Optional self-profiling (PLUME_PROFILE=1): time spent in our own graph code,
// separate from GTK's rendering, printed every 5 s.
thread_local! {
    static PROF: Cell<Option<(i64, u64, i64, u64, i64, u64, i64)>> = const { Cell::new(None) };
}

fn prof_enabled() -> bool {
    thread_local! { static ON: bool = std::env::var_os("PLUME_PROFILE").is_some(); }
    ON.with(|o| *o)
}

/// (ticks, tick_us, rebuilds, rebuild_us, snapshots, snapshot_us)
fn prof_add(tick_us: i64, rebuild_us: Option<i64>, snapshot_us: Option<i64>) {
    PROF.with(|p| {
        let now = glib::monotonic_time();
        let (start, ticks, tus, rebuilds, rus, snaps, sus) = p.get().unwrap_or((now, 0, 0, 0, 0, 0, 0));
        let mut v = (start, ticks, tus, rebuilds, rus, snaps, sus);
        if tick_us >= 0 {
            v.1 += 1;
            v.2 += tick_us;
        }
        if let Some(r) = rebuild_us {
            v.3 += 1;
            v.4 += r;
        }
        if let Some(sn) = snapshot_us {
            v.5 += 1;
            v.6 += sn;
        }
        let elapsed = now - start;
        if elapsed >= 5_000_000 {
            let secs = elapsed as f64 / 1e6;
            let own = (v.2 + v.4 + v.6) as f64 / elapsed as f64 * 100.0;
            eprintln!(
                "plume-profile: {:.0} graph ticks/s ({:.1} µs avg), {:.1} rebuilds/s ({:.0} µs avg), {:.0} snapshots/s ({:.1} µs avg) → graph code = {:.2} % of one core",
                v.1 as f64 / secs,
                v.2 as f64 / v.1.max(1) as f64,
                v.3 as f64 / secs,
                v.4 as f64 / v.3.max(1) as f64,
                v.5 as f64 / secs,
                v.6 as f64 / v.5.max(1) as f64,
                own
            );
            v = (now, 0, 0, 0, 0, 0, 0);
        }
        p.set(Some(v));
    });
}

pub fn set_fps_cap(fps: u32) {
    MIN_FRAME_US.with(|c| c.set(if fps == 0 { 0 } else { 1_000_000 / fps as i64 - 1500 }));
}
pub fn set_history_secs(s: u32) {
    HISTORY_US.with(|c| c.set(s as i64 * 1_000_000));
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
pub fn set_smooth(on: bool) {
    SMOOTH.with(|c| c.set(on));
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Graph {
        pub lines: RefCell<Vec<Line>>,
        pub scale: Cell<Option<Scale>>,
        pub grid: Cell<bool>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub tex: RefCell<Option<gdk::Texture>>,
        pub built_gens: RefCell<Vec<u64>>,
        pub built_disp: Cell<i64>,
        pub built_size: Cell<(i32, i32)>,
        pub built_scale: Cell<f64>,
        pub built_theme: Cell<u64>,
        pub built_window: Cell<i64>,
        pub tex_w: Cell<f32>,
        pub ymax: Cell<f32>,
        pub delay: Cell<i64>,
        pub disp: Cell<i64>,
        pub last_frame: Cell<i64>,
        pub pts: RefCell<Vec<Vec<(i64, f32)>>>,
        pub on_ymax: RefCell<Option<Box<dyn Fn(f32)>>>,
        /// Per-graph time scale (floats); None = follow the global setting.
        pub window_override: Cell<Option<i64>>,
        pub last_rebuild: Cell<i64>,
        pub last_off_px: Cell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Graph {
        const NAME: &'static str = "PlumeGraph";
        type Type = super::Graph;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Graph {
        fn constructed(&self) {
            self.parent_constructed();
            self.grid.set(true);
            self.ymax.set(1.0);
            self.obj().add_css_class("plume-graph");
            self.obj().set_overflow(gtk::Overflow::Hidden);
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
            // Drop the GPU texture while hidden.
            self.tex.replace(None);
            self.built_gens.borrow_mut().clear();
            self.parent_unmap();
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (40, 80, -1, -1),
                _ => (36, 120, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let t0 = if prof_enabled() { glib::monotonic_time() } else { 0 };
            self.draw(snapshot);
            if prof_enabled() {
                prof_add(-1, None, Some(glib::monotonic_time() - t0));
            }
        }
    }

    impl Graph {
        fn draw(&self, snapshot: &gtk::Snapshot) {
            let w = self.obj().width() as f32;
            let h = self.obj().height() as f32;
            if w <= 1.0 || h <= 1.0 {
                return;
            }
            let p = theme::palette();
            let bounds = graphene::Rect::new(0.0, 0.0, w, h);
            snapshot.push_clip(&bounds);

            // Grid: 3 horizontal rules + scrolling verticals every 10 s.
            if self.grid.get() {
                let grid = theme::rgba(p.fg, if p.dark { 0.07 } else { 0.09 });
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
                // Never scroll past the end of the texture (late sample).
                let off = off.clamp(0.0, (tw - w).max(0.0));
                // Pixel-exact: the texture is drawn at exactly its own pixel
                // size and the scroll offset snaps to device pixels, so line
                // edges land on the pixel grid instead of being resampled
                // (which softens them and shimmers while scrolling).
                let s = self.built_scale.get().max(1.0) as f32;
                let off = (off * s).round() / s;
                let (rw, rh) = (tex.width() as f32 / s, tex.height() as f32 / s);
                let filter = if s.fract() == 0.0 { gsk::ScalingFilter::Nearest } else { gsk::ScalingFilter::Linear };
                snapshot.append_scaled_texture(tex, filter, &graphene::Rect::new(-off, 0.0, rw, rh));
            }
            snapshot.pop();
        }

        fn on_tick(&self, frame_us: i64) {
            let t0 = if prof_enabled() { glib::monotonic_time() } else { 0 };
            let rebuilt = self.tick_inner(frame_us);
            if prof_enabled() {
                let total = glib::monotonic_time() - t0;
                prof_add(total - rebuilt.unwrap_or(0), rebuilt, None);
            }
        }

        /// Returns the rebuild time in µs if a rebuild happened.
        fn tick_inner(&self, frame_us: i64) -> Option<i64> {
            let min = MIN_FRAME_US.with(Cell::get);
            if min > 0 && frame_us - self.last_frame.get() < min {
                return None;
            }
            self.last_frame.set(frame_us);
            let obj = self.obj();
            let size = (obj.width(), obj.height());
            if size.0 <= 1 || size.1 <= 1 {
                return None;
            }
            let scale = obj.native().and_then(|n| n.surface()).map(|s| s.scale()).unwrap_or(1.0);
            let window = self.window_us();

            // Has anything we draw changed? (structural changes always rebuild)
            let structural = size != self.built_size.get()
                || scale != self.built_scale.get()
                || theme::generation() != self.built_theme.get()
                || window != self.built_window.get();
            let mut changed = structural;
            let mut new_data = false;
            let mut interval = 0i64;
            crate::app::with_shared(|s| {
                let lines = self.lines.borrow();
                let mut gens = self.built_gens.borrow_mut();
                if gens.len() != lines.len() {
                    gens.resize(lines.len(), u64::MAX);
                    changed = true;
                }
                for (l, g) in lines.iter().zip(gens.iter()) {
                    if let Some(series) = l.key.series(&s.hist) {
                        if series.gen != *g {
                            new_data = true;
                        }
                        interval = interval.max(series.interval_us().unwrap_or(0));
                    }
                }
            });
            // Run one sample interval (plus slack) behind real time.
            if interval > 0 {
                let target = interval + interval / 8 + 8_000;
                let d = self.delay.get();
                // Adapt quickly when the rate changes, otherwise hold steady.
                let d = if d == 0 || (target - d).abs() > d / 4 { target } else { d };
                self.delay.set(d);
            }
            let disp = frame_us - self.delay.get();
            self.disp.set(disp);

            // Rebuild if data changed, or we're about to run off the texture.
            // On long time scales a new sample moves the curve by a fraction of
            // a pixel, so new data only triggers a rebuild once the curve has
            // moved at least one device pixel (or after 1 s).
            let tw = self.tex_w.get();
            let pps = size.0 as f64 / window.max(1) as f64;
            let off = (disp - self.built_disp.get()) as f64 * pps;
            if new_data {
                let moved_px = off * scale;
                let since = frame_us - self.last_rebuild.get();
                if moved_px >= 1.0 || since >= 1_000_000 || window <= 120_000_000 {
                    changed = true;
                }
            }
            let mut rebuilt = None;
            if changed || self.tex.borrow().is_none() || off as f32 > (tw - size.0 as f32) * 0.8 {
                let t0 = glib::monotonic_time();
                self.rebuild(size, scale, window, disp, interval);
                self.last_rebuild.set(frame_us);
                rebuilt = Some(glib::monotonic_time() - t0);
                obj.queue_draw();
            } else {
                // Skip frames that would be pixel-identical (slow scroll).
                let off_px = ((disp - self.built_disp.get()) as f64 * pps * scale).round() as i64;
                if off_px != self.last_off_px.get() {
                    self.last_off_px.set(off_px);
                    obj.queue_draw();
                }
            }
            rebuilt
        }

        pub(super) fn window_us(&self) -> i64 {
            self.window_override.get().unwrap_or_else(|| HISTORY_US.with(Cell::get))
        }

        fn rebuild(&self, (w, h): (i32, i32), scale: f64, window: i64, disp: i64, interval: i64) {
            let obj = self.obj();
            let Some(renderer) = obj.native().and_then(|n| n.renderer()) else { return };
            let (wf, hf) = (w as f32, h as f32);
            let pps = wf as f64 / window.max(1) as f64;
            // Extra width: enough to keep scrolling ~2.5 intervals until the next sample.
            let margin = ((interval.max(50_000) as f64 * 2.5 + 150_000.0) * pps).ceil() as f32 + 4.0;
            let tw = wf + margin;
            let since = disp - window - interval.max(100_000) * 2;

            let lines = self.lines.borrow().clone();
            let mut pts = self.pts.borrow_mut();
            pts.resize_with(lines.len(), Vec::new);
            let mut gens = Vec::with_capacity(lines.len());
            crate::app::with_shared(|s| {
                for (l, buf) in lines.iter().zip(pts.iter_mut()) {
                    match l.key.series(&s.hist) {
                        Some(series) => {
                            series.copy_since(since, buf);
                            gens.push(series.gen);
                        }
                        None => {
                            buf.clear();
                            gens.push(0);
                        }
                    }
                }
            });

            // More points than pixels (long history, high sample rate)? Keep
            // each pixel column's peak so rebuild cost is bounded by width.
            let px_per_us = pps as f64;
            for buf in pts.iter_mut() {
                decimate(buf, px_per_us, disp, wf as f64);
            }

            // Y range.
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
                    // Expand instantly (no clipping), shrink gently.
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
                        // Catmull-Rom through every sample, control points clamped
                        // to each segment's y-range so curves never overshoot.
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
                        // Default: straight segments, so peaks stay sharp.
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
                        let alpha = if p.dark { 0.28 } else { 0.22 };
                        snap.append_fill(&b.to_path(), gsk::FillRule::Winding, &theme::rgba(color, alpha));
                    } else {
                        let stroke = gsk::Stroke::new(1.5 * s);
                        stroke.set_line_join(gsk::LineJoin::Miter);
                        stroke.set_miter_limit(5.0);
                        stroke.set_line_cap(gsk::LineCap::Butt);
                        snap.append_stroke(&b.to_path(), &stroke, &theme::rgba(color, 1.0));
                    }
                }
            }
            drop(pts);
            let viewport = graphene::Rect::new(0.0, 0.0, (tw * s).ceil(), (hf * s).ceil());
            let tex = match snap.to_node() {
                Some(node) => renderer.render_texture(&node, Some(&viewport)),
                None => {
                    self.tex.replace(None);
                    self.finish(gens, (w, h), scale, window, disp, tw);
                    return;
                }
            };
            self.tex.replace(Some(tex));
            self.finish(gens, (w, h), scale, window, disp, tw);
        }

        fn finish(&self, gens: Vec<u64>, size: (i32, i32), scale: f64, window: i64, disp: i64, tw: f32) {
            *self.built_gens.borrow_mut() = gens;
            self.built_size.set(size);
            self.built_scale.set(scale);
            self.built_theme.set(theme::generation());
            self.built_window.set(window);
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
        // 1000 points over 100 px → ≤ ~101 points, peak preserved.
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

    /// Called with the current axis maximum whenever an auto-scaled graph rebuilds.
    pub fn connect_ymax(&self, f: impl Fn(f32) + 'static) {
        self.imp().on_ymax.replace(Some(Box::new(f)));
    }
}
