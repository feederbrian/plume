//! Floating readouts: small borderless windows with one live graph each.
//!
//! On GNOME Wayland an application cannot keep its own window above others or
//! choose its position; that is compositor policy. The companion GNOME Shell
//! extension (extension/plume-floats@feederbrian.github.io) recognises these windows
//! by their title prefix and makes them always-on-top, sticky across
//! workspaces, and restores their last position. Without it they behave as
//! normal windows (and "Always on Top" is still in the Alt+Space menu).
//!
//! An invisible grip in the bottom-right corner resizes a readout; the size
//! is remembered per readout.

use crate::widgets::{Color, Graph, Key, Line, Scale};
use crate::{app, format, settings};
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use plume_core::gpu::PowerState;
use plume_core::Shared;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

/// Window-title prefix the shell extension matches on. Keep in sync with extension.js.
pub const TITLE_PREFIX: &str = "Plume Float · ";

/// Side of the invisible resize grip in the bottom-right corner, logical px.
const GRIP_PX: i32 = 16;

thread_local! {
    static OPEN: RefCell<BTreeMap<String, gtk::Window>> = RefCell::new(BTreeMap::new());
    static GRAPHS: RefCell<BTreeMap<String, Vec<Graph>>> = RefCell::new(BTreeMap::new());
    /// The CPU readout's view switch (total / logical CPUs), while it is open.
    static CPU_VIEW: RefCell<glib::WeakRef<gtk::Stack>> = RefCell::new(glib::WeakRef::new());
}

/// Switches the CPU readout between the total and every logical CPU.
pub fn set_cpu_logical(on: bool) {
    if let Some(st) = CPU_VIEW.with(|v| v.borrow().upgrade()) {
        st.set_visible_child_name(if on { "logical" } else { "total" });
    }
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
    crate::widgets::card::fixed_width(&value, value_chars(id));
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_width_chars(1);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&value);

    let mut graphs: Vec<Graph> = Vec::new();
    let default_size: (i32, i32);
    let body: gtk::Widget = if id == "cores" {
        let (grid, cores, height) = cores_grid(n_cores);
        graphs.extend(cores);
        default_size = (300, height);
        grid.upcast()
    } else {
        let (lines, scale) = lines_for(id, gpu_idx);
        let g = Graph::new(&lines, scale);
        g.set_size_request(80, 32);
        g.set_margin_start(6);
        g.set_margin_end(6);
        g.set_margin_bottom(6);
        default_size = (260, 124);
        graphs.push(g.clone());
        if id == "cpu" {
            // The CPU readout can also show every logical CPU (right-click menu).
            // The hidden page's graphs aren't mapped, so they cost nothing.
            let (grid, cores, _) = cores_grid(n_cores);
            graphs.extend(cores);
            let stack = gtk::Stack::new();
            stack.set_hhomogeneous(false);
            stack.set_vhomogeneous(false);
            stack.add_named(&g, Some("total"));
            stack.add_named(&grid, Some("logical"));
            let logical = settings::with(|s| s.float_cpu_logical);
            stack.set_visible_child_name(if logical { "logical" } else { "total" });
            CPU_VIEW.with(|v| v.borrow().set(Some(&stack)));
            stack.upcast()
        } else {
            g.upcast()
        }
    };

    // Each float has its own time scale; scrolling over it changes it.
    let secs = scale_of(id);
    for g in &graphs {
        g.set_window_secs(Some(secs));
        let fid = id.to_string();
        g.connect_zoom(false, move |dir| set_scale(&fid, crate::widgets::graph::step_scale(scale_of(&fid), dir)));
    }
    GRAPHS.with(|m| m.borrow_mut().insert(id.to_string(), graphs));

    let size = settings::with(|s| s.float_sizes.get(id).copied()).unwrap_or(default_size);
    win.set_default_size(size.0, size.1);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&body);

    // The bottom-right corner resizes. The compositor runs the resize
    // (xdg_toplevel.resize on Wayland), so it is as smooth as any window's.
    let grip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    grip.set_size_request(GRIP_PX, GRIP_PX);
    grip.set_halign(gtk::Align::End);
    grip.set_valign(gtk::Align::End);
    grip.set_cursor_from_name(Some("se-resize"));
    let press = gtk::GestureClick::new();
    press.set_button(gdk::BUTTON_PRIMARY);
    press.connect_pressed(|g, _, x, y| begin_resize(g, x, y));
    grip.add_controller(press);

    let layers = gtk::Overlay::new();
    layers.set_child(Some(&content));
    layers.add_overlay(&grip);
    // GtkWindow clips hit-testing to its rounded corners, which would leave
    // the very corner, where people grab, dead. Nothing inside reaches the
    // corners (everything has margins), so this changes no pixels.
    win.set_overflow(gtk::Overflow::Visible);

    // Dragging anywhere else moves the window (xdg_toplevel.move on Wayland).
    let handle = gtk::WindowHandle::new();
    handle.set_child(Some(&layers));
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
    if id == "cpu" {
        menu.append(Some("Logical CPUs"), Some("app.float-cpu-logical"));
    }
    let close_item = gio::MenuItem::new(Some("Close"), None);
    close_item.set_action_and_target_value(Some("app.float"), Some(&id.to_variant()));
    menu.append_item(&close_item);
    let pop = gtk::PopoverMenu::from_model(Some(&menu));
    pop.set_parent(&content);
    pop.set_has_arrow(false);
    // On the overlay, so the grip's corner gets the menu too (and never the
    // window manager's menu, which GtkWindowHandle would show).
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    click.connect_pressed(glib::clone!(
        #[weak]
        pop,
        #[weak]
        content,
        move |g, _, x, y| {
            let at = g.widget().and_then(|w| w.compute_point(&content, &graphene::Point::new(x as f32, y as f32)));
            let (x, y) = at.map_or((x, y), |p| (p.x() as f64, p.y() as f64));
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop.popup();
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    ));
    layers.add_controller(click);
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gdk::BUTTON_PRIMARY);
    dbl.connect_pressed(|g, n, _, _| {
        if n == 2 {
            // Claimed, or GtkWindowHandle would also take the double-click
            // and maximize the readout.
            g.set_state(gtk::EventSequenceState::Claimed);
            app::show_main();
        }
    });
    layers.add_controller(dbl);
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
    // Remember a size the user sets. Debounced: a resize reports a new size
    // every frame, and each save rewrites the settings file.
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let save_size: Rc<dyn Fn(&gtk::Window)> = {
        let id = id.to_string();
        Rc::new(move |w: &gtk::Window| {
            let (wd, ht) = w.default_size();
            if wd <= 0 || ht <= 0 || w.is_maximized() || w.is_fullscreen() {
                return;
            }
            let new = ((wd, ht) != default_size).then_some((wd, ht));
            if settings::with(|s| s.float_sizes.get(&id).copied()) != new {
                settings::update(|s| match new {
                    Some(v) => {
                        s.float_sizes.insert(id.clone(), v);
                    }
                    None => {
                        s.float_sizes.remove(&id);
                    }
                });
            }
        })
    };
    // Too narrow for the title beside the value: drop the title rather than
    // show a lone "…". The value is what matters in a small readout.
    let fit_title = {
        let (title, value) = (title.clone(), value.clone());
        let title_w = Cell::new(0);
        move |w: &gtk::Window| {
            if title.is_visible() {
                title_w.set(title.measure(gtk::Orientation::Horizontal, -1).1);
            }
            let need = title_w.get() + 6 + value.measure(gtk::Orientation::Horizontal, -1).1;
            let fits = w.default_size().0 >= need;
            title.set_visible(fits);
            value.set_hexpand(!fits); // keeps the value at the right edge alone
        }
    };
    fit_title(&win);
    let on_size: Rc<dyn Fn(&gtk::Window)> = {
        let (pending, save_size) = (pending.clone(), save_size.clone());
        Rc::new(move |w: &gtk::Window| {
            fit_title(w);
            if let Some(src) = pending.take() {
                src.remove();
            }
            let (later, save_size, weak) = (pending.clone(), save_size.clone(), w.downgrade());
            let src = glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                later.take(); // this source is done; forget its id
                if let Some(w) = weak.upgrade() {
                    save_size(&w);
                }
            });
            pending.replace(Some(src));
        })
    };
    {
        let f = on_size.clone();
        win.connect_default_width_notify(move |w| f(w));
    }
    win.connect_default_height_notify(move |w| on_size(w));

    win.connect_close_request(move |w| {
        if let Some(src) = pending.take() {
            src.remove();
            save_size(w);
        }
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

/// A grid with one graph per logical CPU; returns it, its graphs and a
/// default window height that fits it.
fn cores_grid(n_cores: usize) -> (gtk::Grid, Vec<Graph>, i32) {
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
    let mut graphs = Vec::with_capacity(n_cores);
    for i in 0..n_cores {
        let g = Graph::new(&[Line { key: Key::Core(i), color: Color::Core(i), fill: true }], Scale::Unit);
        g.set_grid(false);
        g.set_size_request(24, 16);
        grid.attach(&g, (i % cols) as i32, (i / cols) as i32, 1, 1);
        graphs.push(g);
    }
    (grid, graphs, 36 + 34 * n_cores.div_ceil(cols) as i32 + 10)
}

/// Header value width in characters: room for the longest value, so the text
/// never resizes anything.
fn value_chars(id: &str) -> i32 {
    match id {
        "mem" | "net" | "disk" => 16, // "45 % · 15.6 GiB", "↓1023K ↑1023K"
        _ => 7,                       // "100 %", "Asleep"
    }
}

/// Starts a compositor-driven resize from the bottom-right corner.
fn begin_resize(g: &gtk::GestureClick, x: f64, y: f64) {
    let Some(w) = g.widget() else { return };
    let Some(native) = w.native() else { return };
    let Some(top) = native.surface().and_downcast::<gdk::Toplevel>() else { return };
    let Some(p) = w.compute_point(&native, &graphene::Point::new(x as f32, y as f32)) else { return };
    let (sx, sy) = native.surface_transform();
    g.set_state(gtk::EventSequenceState::Claimed);
    top.begin_resize(
        gdk::SurfaceEdge::SouthEast,
        g.current_event_device().as_ref(),
        g.current_button() as i32,
        p.x() as f64 + sx,
        p.y() as f64 + sy,
        g.current_event_time(),
    );
    // The compositor owns the pointer until the button is released.
    g.reset();
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
