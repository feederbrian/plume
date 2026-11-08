//! Preferences dialog.

use crate::{app, settings, theme, widgets::graph};
use adw::prelude::*;
use gtk::glib;

fn combo<T: Copy + PartialEq + 'static>(
    title: &str,
    subtitle: &str,
    options: &[(&str, T)],
    current: T,
    on_change: impl Fn(T) + 'static,
) -> adw::ComboRow {
    let labels: Vec<&str> = options.iter().map(|(l, _)| *l).collect();
    let row = adw::ComboRow::new();
    row.set_title(title);
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.set_model(Some(&gtk::StringList::new(&labels)));
    let idx = options.iter().position(|(_, v)| *v == current).unwrap_or(0);
    row.set_selected(idx as u32);
    let values: Vec<T> = options.iter().map(|(_, v)| *v).collect();
    row.connect_selected_notify(move |r| {
        if let Some(v) = values.get(r.selected() as usize) {
            on_change(*v);
        }
    });
    row
}

fn switch(title: &str, subtitle: &str, active: bool, on_change: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::new();
    row.set_title(title);
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.set_active(active);
    row.connect_active_notify(move |r| on_change(r.is_active()));
    row
}

pub fn show(parent: Option<&gtk::Window>) {
    let s = settings::get();
    let dialog = adw::PreferencesDialog::new();

    // ----------------------------------------------------------- Appearance
    let page = adw::PreferencesPage::new();
    page.set_title("Appearance");
    page.set_icon_name(Some("preferences-system-symbolic"));
    let g = adw::PreferencesGroup::new();
    g.set_title("Theme");
    let theme_ids: Vec<&'static str> = theme::THEMES.iter().map(|t| t.id).collect();
    let theme_opts: Vec<(&str, usize)> = theme::THEMES.iter().enumerate().map(|(i, t)| (t.name, i)).collect();
    let cur = theme_ids.iter().position(|id| *id == s.theme).unwrap_or(0);
    g.add(&combo("Color scheme", "", &theme_opts, cur, move |i| {
        let id = theme_ids[i];
        settings::update(|s| s.theme = id.into());
        theme::apply(id);
        app::sync_theme_action(id);
    }));
    g.add(&switch(
        "Pure black backgrounds",
        "For OLED screens: unlit black surfaces with any dark theme",
        s.oled,
        |v| {
            settings::update(|s| s.oled = v);
            theme::apply(&settings::with(|s| s.theme.clone()));
        },
    ));
    page.add(&g);

    let g = adw::PreferencesGroup::new();
    g.set_title("Graphs");
    g.add(&combo(
        "Line width",
        "Thickness of graph lines, in screen pixels",
        &[("Hairline (1 px)", 1.0f64), ("Fine (1.5 px)", 1.5), ("Medium (2 px)", 2.0), ("Bold (3 px)", 3.0)],
        s.line_px,
        |v| {
            settings::update(|s| s.line_px = v);
            graph::set_line_px(v as f32);
        },
    ));
    g.add(&switch("Smooth curves", "Round the line through each sample (off: sharp, exact segments)", s.smooth, |v| {
        settings::update(|s| s.smooth = v);
        graph::set_smooth(v);
    }));
    page.add(&g);

    let g = adw::PreferencesGroup::new();
    g.set_title("Floating Readouts");
    let op = adw::SpinRow::with_range(30.0, 100.0, 5.0);
    op.set_title("Opacity");
    op.set_subtitle("Background opacity of floating windows, in percent");
    op.set_value((s.float_opacity * 100.0).round());
    op.connect_value_notify(|r| {
        let v = r.value() / 100.0;
        settings::update(|s| s.float_opacity = v);
        theme::reload_base();
    });
    g.add(&op);
    page.add(&g);
    dialog.add(&page);

    // ---------------------------------------------------------- Performance
    let page = adw::PreferencesPage::new();
    page.set_title("Performance");
    page.set_icon_name(Some("system-run-symbolic"));
    let g = adw::PreferencesGroup::new();
    g.set_title("Graphs");
    g.set_description(Some(
        "Graphs only redraw when they visibly move. Each frame keeps the GPU awake, \
         so lower rates save battery. Sampling faster than 10–20 Hz adds noise rather than detail.",
    ));
    let fps_opts = [("15 fps", 15u32), ("30 fps", 30), ("60 fps", 60), ("120 fps", 120), ("Display refresh rate", 0)];
    g.add(&combo("Frame rate", "Live graphs in the focused window", &fps_opts, s.fps_cap, |v| {
        settings::update(|s| s.fps_cap = v);
        graph::set_fps_caps(v, settings::with(|s| s.fps_cap_bg));
    }));
    g.add(&combo(
        "Background frame rate",
        "Floating readouts and windows you're not using",
        &fps_opts,
        s.fps_cap_bg,
        |v| {
            settings::update(|s| s.fps_cap_bg = v);
            graph::set_fps_caps(settings::with(|s| s.fps_cap), v);
        },
    ));
    g.add(&combo(
        "Graph motion",
        "Smaller scroll steps look smoother but redraw more often",
        &[("Smoothest", 0.25f64), ("Balanced", 1.0), ("Battery saver", 2.0)],
        s.motion_step,
        |v| {
            settings::update(|s| s.motion_step = v);
            graph::set_motion_step(v as f32);
        },
    ));
    g.add(&combo(
        "Sampling rate",
        "How often CPU, memory, network, disk and GPU are measured",
        &[("4 Hz", 4.0f64), ("10 Hz", 10.0), ("20 Hz", 20.0), ("30 Hz", 30.0)],
        s.sample_hz,
        |v| {
            settings::update(|s| s.sample_hz = v);
            app::sampler().set_fast_hz(v);
        },
    ));
    let scale_opts: Vec<(String, u32)> = graph::SCALES.iter().map(|&x| (graph::scale_label(x), x)).collect();
    let scale_refs: Vec<(&str, u32)> = scale_opts.iter().map(|(l, v)| (l.as_str(), *v)).collect();
    g.add(&combo(
        "Time scale",
        "Span shown by graphs. Also in the header bar, or Ctrl+scroll over a graph",
        &scale_refs,
        s.history_secs,
        app::set_time_scale,
    ));
    page.add(&g);
    let g = adw::PreferencesGroup::new();
    g.set_title("Process List");
    g.add(&combo(
        "Refresh interval",
        "The process scan is the most expensive measurement",
        &[("0.5 seconds", 500u64), ("1 second", 1000), ("2 seconds", 2000), ("5 seconds", 5000)],
        s.proc_interval_ms,
        |v| {
            settings::update(|s| s.proc_interval_ms = v);
            app::sampler().set_proc_interval_ms(v);
        },
    ));
    page.add(&g);
    dialog.add(&page);

    // ------------------------------------------------------------ Processes
    let page = adw::PreferencesPage::new();
    page.set_title("Processes");
    page.set_icon_name(Some("view-list-symbolic"));
    let g = adw::PreferencesGroup::new();
    g.add(&switch(
        "Divide CPU usage by core count",
        "100 % means all cores busy (off: 100 % per core, like top)",
        s.divide_cpu,
        |v| {
            settings::update(|s| s.divide_cpu = v);
            app::reload_processes();
        },
    ));
    g.add(&switch("Show kernel threads", "", s.show_kernel_threads, |v| {
        settings::update(|s| s.show_kernel_threads = v);
        app::reload_processes();
    }));
    g.add(&switch("Confirm before ending processes", "", s.confirm_kill, |v| {
        settings::update(|s| s.confirm_kill = v);
    }));
    page.add(&g);
    dialog.add(&page);

    dialog.present(parent);
    let _ = glib::MainContext::default();
}
