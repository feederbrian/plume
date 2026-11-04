//! Floating readouts: small borderless windows with one live graph each.
//!
//! On GNOME Wayland an application cannot keep its own window above others or
//! choose its position; that is compositor policy. The companion GNOME Shell
//! extension (extension/plume-floats@feederbrian.github.io) recognises these windows
//! by their title prefix and makes them always-on-top, sticky across
//! workspaces, and restores their last position. Without it they behave as
//! normal windows (and "Always on Top" is still in the Alt+Space menu).

use crate::widgets::{Color, Graph, Key, Line, Scale};
use crate::{app, format, settings};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use plume_core::gpu::PowerState;
use plume_core::Shared;
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Window-title prefix the shell extension matches on. Keep in sync with extension.js.
pub const TITLE_PREFIX: &str = "Plume Float · ";

thread_local! {
    static OPEN: RefCell<BTreeMap<String, gtk::Window>> = RefCell::new(BTreeMap::new());
    static GRAPHS: RefCell<BTreeMap<String, Vec<Graph>>> = RefCell::new(BTreeMap::new());
}

/// Gives float `id` its own time scale and remembers it.
pub fn set_scale(id: &str, secs: u32) {
    settings::update(|s| {
        s.float_scales.insert(id.to_string(), secs);
    });
    GRAPHS.with(|g| {
        for gr in g.borrow().get(id).into_iter().flatten() {
            gr.set_window_secs(Some(secs));
        }
    });
}

fn scale_of(id: &str) -> u32 {
    settings::with(|s| s.float_scales.get(id).copied().unwrap_or(s.history_secs))
}

pub fn count() -> usize {
    OPEN.with(|o| o.borrow().len())
}

/// (id, menu label) for every float the hardware supports.
pub fn available() -> Vec<(String, String)> {
    let gpus = app::with_shared(|s| s.info.gpus.clone());
    let mut v = vec![("cpu".into(), "CPU".into()), ("cores".into(), "CPU Cores".into()), ("mem".into(), "Memory".into())];
    for (i, g) in gpus.iter().enumerate() {
        v.push((format!("gpu{i}"), format!("GPU {i} ({})", g.name)));
    }
    v.push(("net".into(), "Network".into()));
    v.push(("disk".into(), "Disk".into()));
    v
}

fn title_for(id: &str) -> String {
    match id {
        "cpu" => "CPU".into(),
        "cores" => "CPU Cores".into(),
        "mem" => "Memory".into(),
        "net" => "Network".into(),
        "disk" => "Disk".into(),
        g if g.starts_with("gpu") => format!("GPU {}", &g[3..]),
        other => other.into(),
    }
}

pub fn toggle(id: &str) {
    if let Some(w) = OPEN.with(|o| o.borrow().get(id).cloned()) {
        w.close();
    } else {
        open(id);
    }
}

pub fn open(id: &str) {
    if let Some(w) = OPEN.with(|o| o.borrow().get(id).cloned()) {
        w.present();
        return;
    }
    let n_cores = app::with_shared(|s| s.hist.cores.len());
    let n_gpus = app::with_shared(|s| s.info.gpus.len());
    let gpu_idx = id.strip_prefix("gpu").and_then(|n| n.parse::<usize>().ok());
    if let Some(i) = gpu_idx {
        if i >= n_gpus {
            return;
        }
    }

    let win = gtk::Window::new();
    win.set_title(Some(&format!("{TITLE_PREFIX}{}", title_for(id))));
    win.set_decorated(false);
    win.add_css_class("plume-float");
    if let Some(a) = app::application() {
        win.set_application(Some(&a));
    }

    // Header row (title · value) above the graph, so lines never run under the text.
    let title = gtk::Label::new(Some(&title_for(id)));
    title.add_css_class("plume-float-title");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.set_xalign(0.0);
    let value = gtk::Label::new(None);
    value.add_css_class("plume-float-value");
    value.set_halign(gtk::Align::End);
    value.set_xalign(1.0);
    crate::widgets::card::fixed_width(&value, 16);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_width_chars(1);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&value);

    let mut graphs: Vec<Graph> = Vec::new();
    let body: gtk::Widget = if id == "cores" {
        let grid = gtk::Grid::new();
        grid.set_row_spacing(3);
        grid.set_column_spacing(3);
        grid.set_row_homogeneous(true);
        grid.set_column_homogeneous(true);
        grid.set_margin_top(2);
        grid.set_margin_start(6);
        grid.set_margin_end(6);
        grid.set_margin_bottom(6);
        let cols = if n_cores > 8 { 4 } else { n_cores.clamp(1, 4) };
        for i in 0..n_cores {
            let g = Graph::new(&[Line { key: Key::Core(i), color: Color::Core(i), fill: true }], Scale::Unit);
            g.set_grid(false);
            g.set_size_request(60, 28);
            grid.attach(&g, (i % cols) as i32, (i / cols) as i32, 1, 1);
            graphs.push(g);
        }
        win.set_default_size(300, 36 + 34 * n_cores.div_ceil(cols) as i32 + 10);
        grid.upcast()
    } else {
        let (lines, scale) = lines_for(id, gpu_idx);
        let g = Graph::new(&lines, scale);
        g.set_size_request(120, 50);
        g.set_margin_start(6);
        g.set_margin_end(6);
        g.set_margin_bottom(6);
        win.set_default_size(260, 124);
        graphs.push(g.clone());
        g.upcast()
    };

    // Each float has its own time scale; scrolling over it changes it.
    let secs = scale_of(id);
    for g in &graphs {
        g.set_window_secs(Some(secs));
        let fid = id.to_string();
        g.connect_zoom(false, move |dir| set_scale(&fid, crate::widgets::graph::step_scale(scale_of(&fid), dir)));
    }
    GRAPHS.with(|m| m.borrow_mut().insert(id.to_string(), graphs));

    let overlay = gtk::Box::new(gtk::Orientation::Vertical, 0);
    overlay.append(&header);
    overlay.append(&body);

    // Dragging anywhere moves the window (xdg_toplevel.move on Wayland).
    let handle = gtk::WindowHandle::new();
    handle.set_child(Some(&overlay));
    win.set_child(Some(&handle));

    // Right-click menu.
    let menu = gio::Menu::new();
    menu.append(Some("Open Plume"), Some("app.show-main"));
    let op = gio::Menu::new();
    for (label, v) in [("100 %", 1.0f64), ("90 %", 0.9), ("75 %", 0.75), ("60 %", 0.6), ("45 %", 0.45)] {
        let it = gio::MenuItem::new(Some(label), None);
        it.set_action_and_target_value(Some("app.float-opacity"), Some(&v.to_variant()));
        op.append_item(&it);
    }
    menu.append_submenu(Some("Opacity"), &op);
    let ts = gio::Menu::new();
    for &secs in crate::widgets::graph::SCALES.iter() {
        let it = gio::MenuItem::new(Some(&crate::widgets::graph::scale_label(secs)), None);
        it.set_action_and_target_value(Some("app.float-scale"), Some(&format!("{id}:{secs}").to_variant()));
        ts.append_item(&it);
    }
    menu.append_submenu(Some("Time Scale"), &ts);
    let close_item = gio::MenuItem::new(Some("Close"), None);
    close_item.set_action_and_target_value(Some("app.float"), Some(&id.to_variant()));
    menu.append_item(&close_item);
    let pop = gtk::PopoverMenu::from_model(Some(&menu));
    pop.set_parent(&overlay);
    pop.set_has_arrow(false);
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    click.connect_pressed(glib::clone!(
        #[weak]
        pop,
        move |g, _, x, y| {
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop.popup();
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    ));
    overlay.add_controller(click);
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gdk::BUTTON_PRIMARY);
    dbl.connect_pressed(|_, n, _, _| {
        if n == 2 {
            app::show_main();
        }
    });
    overlay.add_controller(dbl);
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        win,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                win.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    win.add_controller(keys);

    // Value text at 4 Hz while mapped.
    let id_s = id.to_string();
    let timer: std::rc::Rc<RefCell<Option<glib::SourceId>>> = Default::default();
    {
        let (timer, value, id_s) = (timer.clone(), value.clone(), id_s.clone());
        win.connect_map(move |_| {
            let (value, id_s) = (value.clone(), id_s.clone());
            let tick = move || {
                let t = app::with_shared(|s| value_text(&id_s, gpu_idx, s));
                crate::widgets::card::set(&value, &t);
            };
            tick();
            let src = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                tick();
                glib::ControlFlow::Continue
            });
            if let Some(old) = timer.replace(Some(src)) {
                old.remove();
            }
        });
    }
    {
        let timer = timer.clone();
        win.connect_unmap(move |_| {
            if let Some(src) = timer.take() {
                src.remove();
            }
        });
    }
    win.connect_close_request(move |_| {
        OPEN.with(|o| o.borrow_mut().remove(&id_s));
        GRAPHS.with(|g| g.borrow_mut().remove(&id_s));
        remember();
        app::update_demand();
        glib::Propagation::Proceed
    });
    win.connect_suspended_notify(|_| app::update_demand());

    crate::profile::attach(&win, &format!("float {id}"));
    OPEN.with(|o| o.borrow_mut().insert(id.to_string(), win.clone()));
    remember();
    win.present();
    app::update_demand();
    app::maybe_hint_extension();
}

fn remember() {
    let ids: Vec<String> = OPEN.with(|o| o.borrow().keys().cloned().collect());
    if !app::is_quitting() {
        settings::update(|s| s.floats = ids);
    }
}

pub fn all_suspended() -> bool {
    OPEN.with(|o| o.borrow().values().all(|w| w.is_suspended()))
}

fn lines_for(id: &str, gpu: Option<usize>) -> (Vec<Line>, Scale) {
    match id {
        "cpu" => (
            vec![
                Line { key: Key::Cpu, color: Color::Cpu, fill: true },
                Line { key: Key::CpuKernel, color: Color::Kernel, fill: true },
            ],
            Scale::Unit,
        ),
        "mem" => (
            vec![
                Line { key: Key::Mem, color: Color::Mem, fill: true },
                Line { key: Key::Swap, color: Color::Swap, fill: false },
            ],
            Scale::Unit,
        ),
        "net" => (
            vec![
                Line { key: Key::NetRx, color: Color::Rx, fill: true },
                Line { key: Key::NetTx, color: Color::Tx, fill: false },
            ],
            Scale::Auto { floor: 16.0 * 1024.0 },
        ),
        "disk" => (
            vec![
                Line { key: Key::DiskRead, color: Color::Read, fill: true },
                Line { key: Key::DiskWrite, color: Color::Write, fill: false },
            ],
            Scale::Auto { floor: 64.0 * 1024.0 },
        ),
        _ => {
            let i = gpu.unwrap_or(0);
            (vec![Line { key: Key::GpuBusy(i), color: Color::Gpu, fill: true }], Scale::Unit)
        }
    }
}

fn value_text(id: &str, gpu: Option<usize>, s: &Shared) -> String {
    let avg = |series: &plume_core::history::Series, inst: f64| series.mean_recent(1_000_000).map(|v| v as f64).unwrap_or(inst);
    match id {
        "cpu" | "cores" => format::percent(avg(&s.hist.cpu, s.cpu.total as f64) as f32),
        "mem" => format!("{} · {}", format::percent(s.mem.used_frac()), format::bytes(s.mem.used)),
        "net" => format!(
            "↓{} ↑{}",
            format::rate_short(avg(&s.hist.net_rx, s.net.rx_rate)),
            format::rate_short(avg(&s.hist.net_tx, s.net.tx_rate))
        ),
        "disk" => format!(
            "R {} W {}",
            format::rate_short(avg(&s.hist.disk_read, s.disk.read_rate)),
            format::rate_short(avg(&s.hist.disk_write, s.disk.write_rate))
        ),
        _ => {
            let Some(g) = gpu.and_then(|i| s.gpus.get(i)) else { return String::new() };
            match (g.power, g.polled) {
                (PowerState::Suspended, _) => "Asleep".into(),
                (_, false) => "Idle".into(),
                _ => g.busy.map(format::percent).unwrap_or_else(|| "–".into()),
            }
        }
    }
}
