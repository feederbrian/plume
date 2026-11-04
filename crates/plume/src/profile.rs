//! Frame-pacing profiler (PLUME_PROFILE=1).
//!
//! Hooks a window's GdkFrameClock phases and prints, every 5 s, how long the
//! main thread spent per frame (update = tick callbacks, layout, paint =
//! snapshot + render submission) and how regular the frames were. A smooth
//! 60 Hz animation needs every frame well under 16.7 ms and frame times that
//! advance by one refresh interval each time; anything else shows up here as
//! "late" frames or "gaps".

use gtk::{gdk, glib, prelude::*};
use std::cell::RefCell;
use std::rc::Rc;

pub fn enabled() -> bool {
    thread_local! { static ON: bool = std::env::var_os("PLUME_PROFILE").is_some(); }
    ON.with(|o| *o)
}

#[derive(Default)]
struct Stats {
    label: String,
    t_frame: i64,
    t_phase: i64,
    layout: Vec<i64>,
    paint: Vec<i64>,
    total: Vec<i64>,
    frame_times: Vec<i64>,
    window_start: i64,
}

fn pct(v: &mut [i64], p: f64) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() - 1) as f64 * p).round() as usize]
}

impl Stats {
    fn report(&mut self, now: i64) {
        let secs = (now - self.window_start) as f64 / 1e6;
        let n = self.total.len();
        if n == 0 {
            self.window_start = now;
            return;
        }
        // Frame-time regularity: deltas vs. the median interval.
        let deltas: Vec<i64> = self.frame_times.windows(2).map(|w| w[1] - w[0]).collect();
        let med = pct(&mut deltas.clone(), 0.5).max(1);
        let gaps = deltas.iter().filter(|&&d| d > med * 3 / 2).count();
        let late = self.total.iter().filter(|&&t| t > 16_667).count();
        // GTK's own phase handlers run before ours, so each timestamp marks the
        // *end* of GTK's work in that phase: "update+layout" = tick callbacks and
        // size allocation, "paint" = snapshot + render submission.
        let (l50, l95, lmax) = (pct(&mut self.layout, 0.5), pct(&mut self.layout, 0.95), pct(&mut self.layout, 1.0));
        let (p50, p95, pmax) = (pct(&mut self.paint, 0.5), pct(&mut self.paint, 0.95), pct(&mut self.paint, 1.0));
        let (t50, t95, tmax) = (pct(&mut self.total, 0.5), pct(&mut self.total, 0.95), pct(&mut self.total, 1.0));
        eprintln!(
            "plume-frames[{}]: {:.0} fps (interval {:.1} ms, {gaps} gaps) | main thread per frame: \
             p50 {:.2} / p95 {:.2} / max {:.2} ms, {late} over 16.7 ms | \
             update+layout {:.2}/{:.2}/{:.2}, paint {:.2}/{:.2}/{:.2} ms (p50/p95/max)",
            self.label,
            n as f64 / secs,
            med as f64 / 1000.0,
            t50 as f64 / 1000.0,
            t95 as f64 / 1000.0,
            tmax as f64 / 1000.0,
            l50 as f64 / 1000.0,
            l95 as f64 / 1000.0,
            lmax as f64 / 1000.0,
            p50 as f64 / 1000.0,
            p95 as f64 / 1000.0,
            pmax as f64 / 1000.0,
        );
        let keep_last = self.frame_times.last().copied();
        self.layout.clear();
        self.paint.clear();
        self.total.clear();
        self.frame_times.clear();
        self.frame_times.extend(keep_last);
        self.window_start = now;
    }
}

/// Starts profiling `widget`'s window once it is realized.
pub fn attach(widget: &impl IsA<gtk::Widget>, label: &str) {
    if !enabled() {
        return;
    }
    let label = label.to_string();
    widget.connect_realize(move |w| {
        let Some(clock) = w.frame_clock() else { return };
        hook(&clock, &label);
    });
}

fn hook(clock: &gdk::FrameClock, label: &str) {
    let s = Rc::new(RefCell::new(Stats { label: label.into(), window_start: glib::monotonic_time(), ..Default::default() }));
    let now = glib::monotonic_time;
    {
        let s = s.clone();
        clock.connect_before_paint(move |c| {
            let mut st = s.borrow_mut();
            st.t_frame = now();
            st.frame_times.push(c.frame_time());
        });
    }
    {
        let s = s.clone();
        clock.connect_update(move |_| s.borrow_mut().t_phase = now());
    }
    {
        let s = s.clone();
        clock.connect_layout(move |_| {
            let mut st = s.borrow_mut();
            let t = now();
            let d = t - st.t_phase;
            st.layout.push(d);
            st.t_phase = t;
        });
    }
    {
        let s = s.clone();
        clock.connect_paint(move |_| {
            let mut st = s.borrow_mut();
            let t = now();
            let d = t - st.t_phase;
            st.paint.push(d);
            st.t_phase = t;
        });
    }
    clock.connect_after_paint(move |_| {
        let mut st = s.borrow_mut();
        let t = now();
        let total = t - st.t_frame;
        st.total.push(total);
        if t - st.window_start >= 5_000_000 {
            st.report(t);
        }
    });
}
