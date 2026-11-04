//! Process inspector: a live details window for one process.

use crate::{app, format};
use adw::prelude::*;
use gtk::glib;
use plume_core::actions::Sig;
use plume_core::proc::{inspect, ProcInfo};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

thread_local! {
    static OPEN: RefCell<HashMap<(i32, u64), adw::Window>> = RefCell::new(HashMap::new());
}

pub fn count() -> usize {
    OPEN.with(|o| o.borrow().len())
}

struct Rows {
    rows: Vec<(&'static str, adw::ActionRow)>,
}

impl Rows {
    fn set(&self, key: &str, v: impl AsRef<str>) {
        if let Some((_, r)) = self.rows.iter().find(|(k, _)| *k == key) {
            let v = v.as_ref();
            if r.subtitle().as_deref() != Some(v) {
                r.set_subtitle(v);
            }
        }
    }
}

pub fn open(pid: i32, start: u64) {
    if let Some(w) = OPEN.with(|o| o.borrow().get(&(pid, start)).cloned()) {
        w.present();
        return;
    }
    let Some(p) = app::with_shared(|s| s.procs.iter().find(|p| p.pid == pid && p.start_ticks == start).cloned()) else {
        app::toast("The process has already exited");
        return;
    };

    let win = adw::Window::new();
    win.set_title(Some(&format!("{} — Properties", p.name)));
    win.set_default_size(460, 640);
    if let Some(a) = app::application() {
        win.set_application(Some(&a));
    }

    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new(&p.name, &format!("PID {pid}"));
    header.set_title_widget(Some(&title));
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let stop = gtk::Button::from_icon_name("media-playback-pause-symbolic");
    stop.set_tooltip_text(Some("Stop (pause) process"));
    let cont = gtk::Button::from_icon_name("media-playback-start-symbolic");
    cont.set_tooltip_text(Some("Continue process"));
    let end = gtk::Button::with_label("End");
    end.add_css_class("destructive-action");
    let kill = gtk::Button::with_label("Kill");
    actions.append(&stop);
    actions.append(&cont);
    header.pack_start(&actions);
    header.pack_end(&kill);
    header.pack_end(&end);

    let banner = adw::Banner::new("This process has exited");

    let page = adw::PreferencesPage::new();
    let mut rows = Vec::new();
    let mut group = |title: &str, keys: &[(&'static str, &str)]| {
        let g = adw::PreferencesGroup::new();
        g.set_title(title);
        for (key, label) in keys {
            let r = adw::ActionRow::new();
            r.set_title(label);
            r.set_subtitle("–");
            r.set_subtitle_selectable(true);
            r.add_css_class("property");
            g.add(&r);
            rows.push((*key, r));
        }
        page.add(&g);
    };
    group(
        "Process",
        &[
            ("cmdline", "Command Line"),
            ("exe", "Executable"),
            ("cwd", "Working Directory"),
            ("user", "User"),
            ("state", "Status"),
            ("parent", "Parent"),
            ("started", "Started"),
            ("unit", "Systemd Unit"),
            ("cgroup", "Control Group"),
        ],
    );
    group(
        "Resources",
        &[
            ("cpu", "CPU"),
            ("cputime", "CPU Time"),
            ("mem", "Memory (private)"),
            ("rss", "Resident / Shared"),
            ("anon", "Anonymous / File / Shmem"),
            ("swap", "Swap"),
            ("virt", "Virtual (peak)"),
            ("threads", "Threads"),
            ("fds", "Open Files"),
            ("io", "Disk Read / Write"),
            ("net", "Network Received / Sent (TCP)"),
            ("ctx", "Context Switches (vol / invol)"),
        ],
    );
    group(
        "Scheduling",
        &[("nice", "Nice / Priority"), ("cpus", "Allowed CPUs"), ("oom", "OOM Score"), ("seccomp", "Seccomp")],
    );
    let rows = Rc::new(Rows { rows });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&banner);
    content.append(&page);
    page.set_vexpand(true);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&content));
    win.set_content(Some(&tv));

    let name = p.name.to_string();
    {
        let name = name.clone();
        end.connect_clicked(move |_| crate::pages::processes::request_signal(pid, start, &name, Sig::Term));
    }
    {
        let name = name.clone();
        kill.connect_clicked(move |_| crate::pages::processes::request_signal(pid, start, &name, Sig::Kill));
    }
    {
        let name = name.clone();
        stop.connect_clicked(move |_| crate::pages::processes::request_signal(pid, start, &name, Sig::Stop));
    }
    cont.connect_clicked(move |_| crate::pages::processes::request_signal(pid, start, &name, Sig::Cont));

    let boot = app::with_shared(|s| s.info.boot_time);
    let hz = plume_core::util::clock_ticks();
    let started = glib::DateTime::from_unix_local((boot + p.start_ticks / hz) as i64)
        .ok()
        .and_then(|d| d.format("%a %-d %b, %H:%M:%S").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    rows.set("started", &started);

    let update = {
        let rows = rows.clone();
        let banner = banner.clone();
        let buttons = [stop.clone(), cont.clone(), end.clone(), kill.clone()];
        move || {
            let (cur, ncpu, parent) = app::with_shared(|s| {
                let cur = s.procs.iter().find(|q| q.pid == pid && q.start_ticks == start).cloned();
                let parent = cur
                    .as_ref()
                    .and_then(|c| s.procs.iter().find(|q| q.pid == c.ppid))
                    .map(|q| format!("{} ({})", q.name, q.pid));
                (cur, s.info.cpu.logical, parent)
            });
            let Some(c) = cur else {
                banner.set_revealed(true);
                for b in &buttons {
                    b.set_sensitive(false);
                }
                return false;
            };
            fill(&rows, &c, ncpu, parent);
            true
        }
    };
    update();
    // The timer stops itself once the process exits; track liveness so we
    // never remove an already-destroyed source.
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let t2 = timer.clone();
    let id = glib::timeout_add_local(std::time::Duration::from_millis(1000), move || {
        if update() {
            glib::ControlFlow::Continue
        } else {
            t2.borrow_mut().take();
            glib::ControlFlow::Break
        }
    });
    timer.replace(Some(id));
    win.connect_close_request(move |_| {
        if let Some(id) = timer.take() {
            id.remove();
        }
        OPEN.with(|o| o.borrow_mut().remove(&(pid, start)));
        app::update_demand();
        glib::Propagation::Proceed
    });

    OPEN.with(|o| o.borrow_mut().insert((pid, start), win.clone()));
    app::update_demand();
    win.present();
}

fn fill(rows: &Rows, c: &ProcInfo, ncpu: usize, parent: Option<String>) {
    let d = inspect(c.pid).unwrap_or_default();
    let hz = plume_core::util::clock_ticks();
    let opt = |v: Option<u64>| v.map(format::bytes).unwrap_or_else(|| "–".into());
    rows.set("cmdline", if d.cmdline.is_empty() { format!("[{}]", c.name) } else { d.cmdline.clone() });
    rows.set("exe", d.exe.clone().unwrap_or_else(|| "Not accessible".into()));
    rows.set("cwd", d.cwd.clone().unwrap_or_else(|| "Not accessible".into()));
    rows.set("user", format!("{} (UID {})", c.user, c.uid));
    rows.set("state", format::state(c.state));
    rows.set("parent", parent.unwrap_or_else(|| c.ppid.to_string()));
    rows.set("unit", d.unit.clone().unwrap_or_else(|| "–".into()));
    rows.set("cgroup", d.cgroup.clone().unwrap_or_else(|| "–".into()));
    rows.set(
        "cpu",
        format!("{} of all CPUs · {:.0} % of one core", format::percent(c.cpu / ncpu.max(1) as f32), c.cpu * 100.0),
    );
    rows.set("cputime", format::cpu_time(c.cpu_ticks, hz));
    rows.set("mem", format::bytes(c.memory));
    rows.set("rss", format!("{} / {}", format::bytes(c.rss), format::bytes(c.shared)));
    rows.set("anon", format!("{} / {} / {}", opt(d.rss_anon), opt(d.rss_file), opt(d.rss_shmem)));
    rows.set("swap", opt(d.swap));
    rows.set("virt", format!("{} ({})", opt(d.vm_size), opt(d.vm_peak)));
    rows.set("threads", c.threads.to_string());
    rows.set("fds", d.open_fds.map(|n| n.to_string()).unwrap_or_else(|| "Not accessible".into()));
    rows.set(
        "io",
        match (c.read_rate, c.write_rate) {
            (Some(r), Some(w)) => format!(
                "{} / {} total · {} / {}",
                format::bytes(c.read_total),
                format::bytes(c.write_total),
                format::rate(r as f64),
                format::rate(w as f64)
            ),
            _ => "Not accessible".into(),
        },
    );
    rows.set(
        "net",
        match c.net {
            Some(n) => format!(
                "{} / {} · {} / {} · {} connections",
                format::bytes(n.rx_total),
                format::bytes(n.tx_total),
                format::rate(n.rx_rate as f64),
                format::rate(n.tx_rate as f64),
                n.connections
            ),
            None => "Shown while the Network tab is open".into(),
        },
    );
    rows.set(
        "ctx",
        match (d.ctx_voluntary, d.ctx_involuntary) {
            (Some(v), Some(i)) => format!("{v} / {i}"),
            _ => "–".into(),
        },
    );
    rows.set("nice", format!("{} / {}", c.nice, c.priority));
    rows.set("cpus", d.cpus_allowed.clone().unwrap_or_else(|| "–".into()));
    rows.set("oom", d.oom_score.map(|v| v.to_string()).unwrap_or_else(|| "–".into()));
    rows.set(
        "seccomp",
        match d.seccomp {
            Some(0) => "Disabled",
            Some(1) => "Strict",
            Some(2) => "Filter",
            _ => "–",
        },
    );
}
