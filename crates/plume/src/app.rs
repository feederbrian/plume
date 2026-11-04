//! Application shell: globals, main window, actions, command line.

use crate::pages::processes::{self, Channel};
use crate::{floats, settings, theme, widgets::graph, APP_ID};
use adw::prelude::*;
use gtk::{gio, glib};
use plume_core::{Sampler, SamplerConfig, Shared};
use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

pub const EXTENSION_UUID: &str = "plume-floats@feederbrian.github.io";

static SAMPLER: OnceLock<Sampler> = OnceLock::new();

thread_local! {
    static APP: RefCell<Option<adw::Application>> = const { RefCell::new(None) };
    static MAIN: RefCell<Option<MainWindow>> = const { RefCell::new(None) };
    static QUITTING: Cell<bool> = const { Cell::new(false) };
    static HINTED: Cell<bool> = const { Cell::new(false) };
    static EXT_ACTIVE: Cell<Option<bool>> = const { Cell::new(None) };
    static SCALE_DROPDOWN: RefCell<Option<gtk::DropDown>> = const { RefCell::new(None) };
    static SCALE_SYNCING: Cell<bool> = const { Cell::new(false) };
}

/// Sets the global time scale of all graphs (floats with their own scale excepted).
pub fn set_time_scale(secs: u32) {
    settings::update(|s| s.history_secs = secs);
    graph::set_history_secs(secs);
    SCALE_DROPDOWN.with(|d| {
        if let Some(d) = d.borrow().as_ref() {
            if let Some(i) = graph::SCALES.iter().position(|&x| x == secs) {
                SCALE_SYNCING.with(|f| f.set(true));
                d.set_selected(i as u32);
                SCALE_SYNCING.with(|f| f.set(false));
            }
        }
    });
}

pub fn step_time_scale(dir: i32) {
    let cur = settings::with(|s| s.history_secs);
    set_time_scale(graph::step_scale(cur, dir));
}

/// Ctrl+scroll over a main-window graph changes the global time scale.
pub fn zoomable(g: &graph::Graph) {
    g.connect_zoom(true, step_time_scale);
}

fn time_scale_dropdown() -> gtk::DropDown {
    let labels: Vec<String> = graph::SCALES.iter().map(|&s| graph::scale_label(s)).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let d = gtk::DropDown::from_strings(&refs);
    d.set_tooltip_text(Some("Time scale of the graphs (Ctrl+scroll over a graph to zoom)"));
    let cur = settings::with(|s| s.history_secs);
    let idx = graph::SCALES.iter().position(|&x| x >= cur).unwrap_or(1);
    d.set_selected(idx as u32);
    d.connect_selected_notify(|d| {
        if SCALE_SYNCING.with(Cell::get) {
            return;
        }
        if let Some(&secs) = graph::SCALES.get(d.selected() as usize) {
            set_time_scale(secs);
        }
    });
    SCALE_DROPDOWN.with(|c| c.replace(Some(d.clone())));
    d
}

#[derive(Clone)]
struct MainWindow {
    win: adw::ApplicationWindow,
    stack: adw::ViewStack,
    toasts: adw::ToastOverlay,
}

pub fn clock_ticks() -> u64 {
    thread_local! { static HZ: u64 = plume_core::util::clock_ticks(); }
    HZ.with(|h| *h)
}

pub fn sampler() -> &'static Sampler {
    SAMPLER.get_or_init(|| {
        let s = settings::get();
        Sampler::start(SamplerConfig { fast_hz: s.sample_hz, proc_interval_ms: s.proc_interval_ms })
    })
}

pub fn with_shared<R>(f: impl FnOnce(&Shared) -> R) -> R {
    f(&sampler().lock())
}

pub fn application() -> Option<adw::Application> {
    APP.with(|a| a.borrow().clone())
}

pub fn is_quitting() -> bool {
    QUITTING.with(Cell::get)
}

pub fn active_window() -> Option<gtk::Window> {
    application().and_then(|a| a.active_window())
}

pub fn toast(msg: &str) {
    match MAIN.with(|m| m.borrow().clone()) {
        Some(m) => m.toasts.add_toast(adw::Toast::new(msg)),
        None => eprintln!("plume: {msg}"),
    }
}

pub fn reload_processes() {
    processes::reload();
}

/// Tells the sampler what is actually on screen, so it only does the work
/// someone is looking at.
pub fn update_demand() {
    let main = MAIN.with(|m| m.borrow().clone());
    let main_visible = main.as_ref().is_some_and(|m| m.win.is_visible() && !m.win.is_suspended());
    let page = main
        .as_ref()
        .filter(|_| main_visible)
        .and_then(|m| m.stack.visible_child_name())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let tab = Channel::from_id(&page);
    let inspecting = crate::inspector::count() > 0;
    // Only pay for what is on screen: /proc/<pid>/io for the Disk tab,
    // socket attribution for the Network tab.
    sampler().set_want_procs(tab.is_some() || inspecting);
    sampler().set_want_io(tab == Some(Channel::Disk) || inspecting);
    sampler().set_want_net(tab == Some(Channel::Network));
    let anything_visible = main_visible || (floats::count() > 0 && !floats::all_suspended());
    sampler().set_idle(!anything_visible && crate::inspector::count() == 0);
}

pub fn show_main() {
    if let Some(m) = MAIN.with(|m| m.borrow().clone()) {
        m.win.present();
        return;
    }
    let Some(app) = application() else { return };
    let m = build_main(&app);
    MAIN.with(|c| c.replace(Some(m.clone())));
    m.win.present();
    update_demand();
}

fn build_main(app: &adw::Application) -> MainWindow {
    let s = settings::get();
    let win = adw::ApplicationWindow::new(app);
    win.set_title(Some("Plume"));
    win.set_default_size(s.width, s.height);
    win.set_icon_name(Some(APP_ID));

    let stack = adw::ViewStack::new();
    for ch in Channel::ALL {
        let p = stack.add_titled(&processes::page(ch), Some(ch.id()), ch.title());
        p.set_icon_name(Some(ch.icon()));
    }
    let r = stack.add_titled(&crate::pages::resources::build(), Some("overview"), "Overview");
    r.set_icon_name(Some("view-grid-symbolic"));
    if stack.child_by_name(&s.page).is_some() {
        stack.set_visible_child_name(&s.page);
    }
    let toolbar = processes::toolbar();
    toolbar.set_visible(stack.visible_child_name().as_deref() != Some("overview"));

    let header = adw::HeaderBar::new();
    let switcher = adw::ViewSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_policy(adw::ViewSwitcherPolicy::Wide);
    header.set_title_widget(Some(&switcher));

    header.pack_start(&time_scale_dropdown());

    let menu_btn = gtk::MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    menu_btn.set_tooltip_text(Some("Main Menu"));
    menu_btn.set_menu_model(Some(&primary_menu()));
    menu_btn.set_primary(true);
    header.pack_end(&menu_btn);

    let float_btn = gtk::MenuButton::new();
    float_btn.set_icon_name("window-new-symbolic");
    float_btn.set_tooltip_text(Some("Floating Readouts"));
    float_btn.set_menu_model(Some(&floats_menu()));
    header.pack_end(&float_btn);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&toolbar);
    body.append(&stack);
    stack.set_vexpand(true);
    body.insert_action_group("proc", Some(&processes::actions()));
    tv.set_content(Some(&body));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&tv));
    win.set_content(Some(&toasts));

    {
        let toolbar = toolbar.clone();
        stack.connect_visible_child_name_notify(move |st| {
            if let Some(n) = st.visible_child_name() {
                toolbar.set_visible(n != "overview");
                settings::update(|s| s.page = n.to_string());
            }
            update_demand();
        });
    }
    win.connect_suspended_notify(|_| update_demand());
    win.connect_close_request(|w| {
        let (wd, ht) = w.default_size();
        if !w.is_maximized() && wd > 0 && ht > 0 {
            settings::update(|s| {
                s.width = wd;
                s.height = ht;
            });
        }
        MAIN.with(|m| m.replace(None));
        update_demand();
        glib::Propagation::Proceed
    });

    // Window shortcuts.
    let find = gio::SimpleAction::new("find", None);
    {
        let (stack, toolbar) = (stack.clone(), toolbar.clone());
        find.connect_activate(move |_, _| {
            if stack.visible_child_name().as_deref() == Some("overview") {
                stack.set_visible_child_name("cpu");
            }
            processes::focus_search(&toolbar);
        });
    }
    win.add_action(&find);
    let show = gio::SimpleAction::new("show-tab", Some(glib::VariantTy::STRING));
    {
        let stack = stack.clone();
        show.connect_activate(move |_, p| {
            if let Some(n) = p.and_then(|v| v.get::<String>()) {
                stack.set_visible_child_name(&n);
            }
        });
    }
    win.add_action(&show);

    MainWindow { win, stack, toasts }
}

fn primary_menu() -> gio::Menu {
    let m = gio::Menu::new();
    let themes = gio::Menu::new();
    for t in theme::THEMES {
        let it = gio::MenuItem::new(Some(t.name), None);
        it.set_action_and_target_value(Some("app.theme"), Some(&t.id.to_variant()));
        themes.append_item(&it);
    }
    let s1 = gio::Menu::new();
    s1.append_submenu(Some("Theme"), &themes);
    m.append_section(None, &s1);
    let s2 = gio::Menu::new();
    s2.append(Some("Preferences"), Some("app.preferences"));
    s2.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    s2.append(Some("About Plume"), Some("app.about"));
    m.append_section(None, &s2);
    let s3 = gio::Menu::new();
    s3.append(Some("Quit"), Some("app.quit"));
    m.append_section(None, &s3);
    m
}

fn floats_menu() -> gio::Menu {
    let m = gio::Menu::new();
    let sec = gio::Menu::new();
    for (id, label) in floats::available() {
        let it = gio::MenuItem::new(Some(&label), None);
        it.set_action_and_target_value(Some("app.float"), Some(&id.to_variant()));
        sec.append_item(&it);
    }
    m.append_section(Some("Show or hide floating readout"), &sec);
    m
}

pub fn sync_theme_action(id: &str) {
    if let Some(a) = application().and_then(|a| a.lookup_action("theme")).and_downcast::<gio::SimpleAction>() {
        a.set_state(&id.to_variant());
    }
}

fn setup_actions(app: &adw::Application) {
    let cur = settings::with(|s| s.theme.clone());
    let theme_a = gio::SimpleAction::new_stateful("theme", Some(glib::VariantTy::STRING), &cur.to_variant());
    theme_a.connect_activate(|a, p| {
        let Some(id) = p.and_then(|v| v.get::<String>()) else { return };
        a.set_state(&id.to_variant());
        settings::update(|s| s.theme = id.clone());
        theme::apply(&id);
    });
    app.add_action(&theme_a);

    let float_a = gio::SimpleAction::new("float", Some(glib::VariantTy::STRING));
    float_a.connect_activate(|_, p| {
        if let Some(id) = p.and_then(|v| v.get::<String>()) {
            floats::toggle(&id);
        }
    });
    app.add_action(&float_a);

    let ts = gio::SimpleAction::new("time-scale", Some(glib::VariantTy::UINT32));
    ts.connect_activate(|_, p| {
        if let Some(secs) = p.and_then(|v| v.get::<u32>()) {
            set_time_scale(secs.clamp(10, 3600));
        }
    });
    app.add_action(&ts);
    for (name, dir) in [("time-scale-longer", 1), ("time-scale-shorter", -1)] {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| step_time_scale(dir));
        app.add_action(&a);
    }

    let fs = gio::SimpleAction::new("float-scale", Some(glib::VariantTy::STRING));
    fs.connect_activate(|_, p| {
        let Some(v) = p.and_then(|v| v.get::<String>()) else { return };
        if let Some((id, secs)) = v.split_once(':') {
            if let Ok(secs) = secs.parse::<u32>() {
                floats::set_scale(id, secs);
            }
        }
    });
    app.add_action(&fs);

    let op = gio::SimpleAction::new("float-opacity", Some(glib::VariantTy::DOUBLE));
    op.connect_activate(|_, p| {
        if let Some(v) = p.and_then(|v| v.get::<f64>()) {
            settings::update(|s| s.float_opacity = v);
            theme::reload_base();
        }
    });
    app.add_action(&op);

    let show = gio::SimpleAction::new("show-main", None);
    show.connect_activate(|_, _| show_main());
    app.add_action(&show);

    let prefs = gio::SimpleAction::new("preferences", None);
    prefs.connect_activate(|_, _| crate::prefs::show(active_window().as_ref()));
    app.add_action(&prefs);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(|_, _| {
        let d = adw::AboutDialog::builder()
            .application_name("Plume")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .comments("A featherweight system monitor with live 60 fps graphs.")
            .license_type(gtk::License::Gpl30)
            .developer_name("Brian Soria")
            .developers(vec!["Brian Soria"])
            .website("https://github.com/feederbrian/plume")
            .issue_url("https://github.com/feederbrian/plume/issues")
            .build();
        let cost = with_shared(|s| (s.fast_cost_us, s.proc_stats.elapsed_us, s.proc_stats.processes));
        d.set_debug_info(&format!(
            "Fast sample: {} µs\nProcess scan: {} µs for {} processes\nGTK {}.{}.{}\nlibadwaita {}.{}.{}\n",
            cost.0,
            cost.1,
            cost.2,
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
            adw::major_version(),
            adw::minor_version(),
            adw::micro_version()
        ));
        d.present(active_window().as_ref());
    });
    app.add_action(&about);

    let shortcuts = gio::SimpleAction::new("shortcuts", None);
    shortcuts.connect_activate(|_, _| {
        let d = adw::AlertDialog::new(
            Some("Keyboard Shortcuts"),
            Some(
                "Ctrl+F\tSearch processes\nCtrl+1 … Ctrl+5\tCPU, Memory, Disk, Network, Overview\n\
                 Delete\tEnd process\nShift+Delete\tKill process\nCtrl+S / Ctrl+C\tStop / continue process\n\
                 Ctrl+I, Enter\tProcess properties\nCtrl+scroll on a graph, Ctrl+− / Ctrl+=\tLonger / shorter time scale\n\
                 Scroll on a floating readout\tChange its time scale\nCtrl+,\tPreferences\nCtrl+Q\tQuit\n\
                 Esc (in a floating readout)\tClose it",
            ),
        );
        d.add_response("ok", "Close");
        d.present(active_window().as_ref());
    });
    app.add_action(&shortcuts);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            QUITTING.with(|q| q.set(true));
            app.quit();
        }
    ));
    app.add_action(&quit);

    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    app.set_accels_for_action("app.time-scale-longer", &["<Control>minus"]);
    app.set_accels_for_action("app.time-scale-shorter", &["<Control>equal", "<Control>plus"]);
    app.set_accels_for_action("win.find", &["<Control>f"]);
    for (i, id) in ["cpu", "memory", "disk", "network", "overview"].iter().enumerate() {
        app.set_accels_for_action(&format!("win.show-tab::{id}"), &[&format!("<Control>{}", i + 1)]);
    }
}

/// Checks (once) whether the always-on-top shell extension is active, and
/// suggests it the first time a floating readout is opened without it.
pub fn maybe_hint_extension() {
    if HINTED.with(Cell::get) {
        return;
    }
    HINTED.with(|h| h.set(true));
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, |conn| {
        let Ok(conn) = conn else { return };
        conn.call(
            Some("org.gnome.Shell.Extensions"),
            "/org/gnome/Shell/Extensions",
            "org.gnome.Shell.Extensions",
            "GetExtensionInfo",
            Some(&(EXTENSION_UUID,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            |res| {
                // state 1 = ENABLED (ACTIVE in GNOME 45+).
                let active = res
                    .ok()
                    .and_then(|v| glib::VariantDict::new(Some(&v.child_value(0))).lookup_value("state", None))
                    .and_then(|s| s.get::<f64>())
                    .is_some_and(|s| s == 1.0);
                EXT_ACTIVE.with(|e| e.set(Some(active)));
                if !active {
                    toast(
                        "To keep floating readouts above other windows, enable the “Plume Floats” \
                         extension (gnome-extensions enable plume-floats@feederbrian.github.io), or use \
                         Alt+Space → Always on Top.",
                    );
                }
            },
        );
    });
}

pub const HELP: &str = "Usage: plume [--float ID]... [--background]

  --float ID     open a floating readout (repeatable)
                 IDs: cpu, cores, mem, gpu0, gpu1, net, disk
  --background   don't open the main window (floats only)
  --version      print version
";

fn handle_command_line(app: &adw::Application, cmd: &gio::ApplicationCommandLine) -> glib::ExitCode {
    let args: Vec<String> = cmd.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let mut floats_req = Vec::new();
    let mut background = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--float" | "-f" => {
                if let Some(v) = it.next() {
                    floats_req.push(v.clone());
                }
            }
            s if s.starts_with("--float=") => floats_req.push(s["--float=".len()..].to_string()),
            "--background" | "-b" => background = true,
            other => {
                eprintln!("plume: unknown option {other} (see plume --help)");
                return glib::ExitCode::FAILURE;
            }
        }
    }
    let first = !app.is_remote() && app.windows().is_empty();
    if first && floats_req.is_empty() {
        floats_req = settings::with(|s| s.floats.clone());
    }
    if !background || (floats_req.is_empty() && app.windows().is_empty()) {
        show_main();
    }
    for f in floats_req {
        floats::open(&f);
    }
    glib::ExitCode::SUCCESS
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|app| {
        APP.with(|a| a.replace(Some(app.clone())));
        let s = settings::get();
        graph::set_fps_cap(s.fps_cap);
        graph::set_history_secs(s.history_secs);
        graph::set_smooth(s.smooth);
        theme::apply(&s.theme);
        theme::follow_system();
        let _ = sampler();
        setup_actions(app);
    });
    app.connect_command_line(handle_command_line);
    app.connect_shutdown(|_| QUITTING.with(|q| q.set(true)));
    app.run()
}
