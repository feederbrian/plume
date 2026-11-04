//! Resources dashboard: CPU, memory, GPUs, network, disk.
//!
//! Graphs animate themselves (see widgets::graph). Text labels refresh at 4 Hz
//! from one timer that only runs while the page is mapped.

use crate::format;
use crate::widgets::card::{self, card, graph_frame, stat};
use crate::widgets::{Color, Graph, Key, Line, Scale};
use gtk::{glib, prelude::*};
use plume_core::gpu::{PowerState, Vendor};
use plume_core::Shared;
use std::cell::RefCell;
use std::rc::Rc;

type Updater = Box<dyn Fn(&Shared)>;

/// 1-second mean of a history series (falls back to the instant value).
fn avg(series: &plume_core::history::Series, instant: f64) -> f64 {
    series.mean_recent(1_000_000).map(|v| v as f64).unwrap_or(instant)
}

pub fn build() -> gtk::Widget {
    let (info, n_cores) = crate::app::with_shared(|s| (s.info.clone(), s.hist.cores.len()));
    let mut updaters: Vec<Updater> = Vec::new();

    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(false);
    flow.set_min_children_per_line(1);
    flow.set_max_children_per_line(2);
    flow.set_column_spacing(12);
    flow.set_row_spacing(12);
    flow.set_valign(gtk::Align::Start);
    flow.set_margin_top(12);
    flow.set_margin_bottom(12);
    flow.set_margin_start(12);
    flow.set_margin_end(12);

    let add = |w: &gtk::Box| {
        w.set_size_request(400, -1);
        let child = gtk::FlowBoxChild::new();
        child.set_child(Some(w));
        child.set_focusable(false);
        flow.append(&child);
    };

    // ---------------------------------------------------------------- CPU
    {
        let c = card("CPU", Some("cpu"));
        c.sub.set_text(&format!(
            "{} · {} cores, {} threads",
            info.cpu.model, info.cpu.physical, info.cpu.logical
        ));
        let combined = Graph::new(
            &[
                Line { key: Key::Cpu, color: Color::Cpu, fill: true },
                Line { key: Key::CpuKernel, color: Color::Kernel, fill: true },
            ],
            Scale::Unit,
        );
        crate::app::zoomable(&combined);
        let (frame, _) = graph_frame(&combined, 150);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(6);
        grid.set_column_spacing(6);
        grid.set_row_homogeneous(true);
        grid.set_column_homogeneous(true);
        let cols = match n_cores {
            0..=4 => n_cores.max(1),
            5..=16 => 4,
            17..=24 => 6,
            _ => 8,
        };
        let mut core_labels = Vec::new();
        for i in 0..n_cores {
            let g = Graph::new(&[Line { key: Key::Core(i), color: Color::Cpu, fill: true }], Scale::Unit);
            crate::app::zoomable(&g);
            let (o, corner) = graph_frame(&g, 46);
            corner.remove_css_class("plume-ymax");
            corner.add_css_class("plume-core-label");
            grid.attach(&o, (i % cols) as i32, (i / cols) as i32, 1, 1);
            core_labels.push(corner);
        }
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&frame, Some("combined"));
        stack.add_named(&grid, Some("cores"));
        let per_core = crate::settings::with(|s| s.cpu_per_core);
        stack.set_visible_child_name(if per_core { "cores" } else { "combined" });
        c.body.append(&stack);

        let toggle = gtk::ToggleButton::new();
        toggle.set_icon_name("view-grid-symbolic");
        toggle.add_css_class("flat");
        toggle.set_tooltip_text(Some("Show each logical CPU"));
        toggle.set_active(per_core);
        toggle.connect_toggled(glib::clone!(
            #[weak]
            stack,
            move |t| {
                let on = t.is_active();
                stack.set_visible_child_name(if on { "cores" } else { "combined" });
                crate::settings::update(|s| s.cpu_per_core = on);
            }
        ));
        c.header_extra.append(&toggle);

        let util = stat(&c, "Utilization", Some("plume-sw-cpu"));
        let kern = stat(&c, "Kernel", Some("plume-sw-kernel"));
        let speed = stat(&c, "Speed", None);
        let temp = stat(&c, "Temperature", None);
        let load = stat(&c, "Load average", None);
        let procs = stat(&c, "Processes", None);
        let threads = stat(&c, "Threads", None);
        let uptime = stat(&c, "Uptime", None);
        let value = c.value.clone();
        updaters.push(Box::new(move |s| {
            let total = avg(&s.hist.cpu, s.cpu.total as f64) as f32;
            card::set(&value, &format::percent(total));
            card::set(&util, &format::percent(total));
            card::set(&kern, &format::percent(avg(&s.hist.cpu_kernel, s.cpu.kernel as f64) as f32));
            let f: Vec<u32> = s.cpu_freq.iter().copied().filter(|&m| m > 0).collect();
            if !f.is_empty() {
                let avg = f.iter().map(|&m| m as u64).sum::<u64>() / f.len() as u64;
                let max = *f.iter().max().unwrap_or(&0);
                card::set(&speed, &format!("{} (max {})", format::mhz(avg as u32), format::mhz(max)));
            }
            if let Some(t) = s.cpu_temp {
                card::set(&temp, &format!("{t:.0} °C"));
            }
            card::set(&load, &format!("{:.2}  {:.2}  {:.2}", s.load[0], s.load[1], s.load[2]));
            if s.n_procs > 0 {
                card::set(&procs, &s.n_procs.to_string());
                card::set(&threads, &s.n_threads.to_string());
            }
            card::set(&uptime, &format::duration(s.uptime));
            for (i, l) in core_labels.iter().enumerate() {
                if let Some(v) = s.cpu.cores.get(i) {
                    card::set(l, &format!("CPU{i}  {}", format::percent(*v)));
                }
            }
        }));
        add(&c.root);
    }

    // ------------------------------------------------------------- Memory
    {
        let c = card("Memory", Some("mem"));
        let g = Graph::new(
            &[
                Line { key: Key::Mem, color: Color::Mem, fill: true },
                Line { key: Key::Swap, color: Color::Swap, fill: false },
            ],
            Scale::Unit,
        );
        crate::app::zoomable(&g);
        let (frame, _) = graph_frame(&g, 150);
        c.body.append(&frame);
        let used = stat(&c, "Used", Some("plume-sw-mem"));
        let avail = stat(&c, "Available", None);
        let cache = stat(&c, "Cache", None);
        let free = stat(&c, "Free", None);
        let swap = stat(&c, "Swap", Some("plume-sw-swap"));
        let shmem = stat(&c, "Shared", None);
        let (value, sub) = (c.value.clone(), c.sub.clone());
        updaters.push(Box::new(move |s| {
            let m = &s.mem;
            card::set(&value, &format::percent(m.used_frac()));
            card::set(&sub, &format!("{} of {}", format::bytes(m.used), format::bytes(m.total)));
            card::set(&used, &format::bytes(m.used));
            card::set(&avail, &format::bytes(m.available));
            card::set(&cache, &format::bytes(m.cache));
            card::set(&free, &format::bytes(m.free));
            card::set(&shmem, &format::bytes(m.shmem));
            if m.swap_total > 0 {
                card::set(&swap, &format!("{} of {}", format::bytes(m.swap_used), format::bytes(m.swap_total)));
            } else {
                card::set(&swap, "None");
            }
        }));
        add(&c.root);
    }

    // --------------------------------------------------------------- GPUs
    for (i, gi) in info.gpus.iter().enumerate() {
        let c = card(&format!("GPU {i}"), Some(&format!("gpu{i}")));
        let kind = if gi.integrated { "Integrated" } else { "Discrete" };
        c.sub.set_text(&format!("{} · {kind}", gi.name));
        let mut lines = vec![Line { key: Key::GpuBusy(i), color: Color::Gpu, fill: true }];
        if !gi.integrated {
            lines.push(Line { key: Key::GpuMem(i), color: Color::Mem, fill: false });
        }
        let g = Graph::new(&lines, Scale::Unit);
        crate::app::zoomable(&g);
        let (frame, _) = graph_frame(&g, 150);
        c.body.append(&frame);
        let util = stat(&c, "Utilization", Some("plume-sw-gpu"));
        let state = stat(&c, "State", None);
        let freq = stat(&c, "Frequency", None);
        let vram = (!gi.integrated).then(|| stat(&c, "Video memory", Some("plume-sw-mem")));
        let temp = (gi.vendor != Vendor::Intel).then(|| stat(&c, "Temperature", None));
        let power = (gi.vendor != Vendor::Intel).then(|| stat(&c, "Power", None));
        let drv = stat(&c, "Driver", None);
        drv.set_text(&gi.driver);
        drv.set_tooltip_text(Some(&format!("Utilization source: {}", gi.busy_source)));
        let (value, sub, base_name) = (c.value.clone(), c.sub.clone(), gi.name.clone());
        updaters.push(Box::new(move |s| {
            let Some(g) = s.gpus.get(i) else { return };
            if let Some(n) = &g.driver_name {
                card::set(&sub, &format!("{n} · {kind}"));
            } else {
                card::set(&sub, &format!("{base_name} · {kind}"));
            }
            let asleep = g.power == PowerState::Suspended;
            let (v, st) = match (g.power, g.polled) {
                (PowerState::Suspended, _) => ("Asleep".to_string(), "Powered down (saving battery)"),
                (_, false) => ("Idle".to_string(), "Awake, not polled so it can sleep"),
                _ => (g.busy.map(format::percent).unwrap_or_else(|| "–".into()), "Active"),
            };
            card::set(&value, &v);
            if asleep {
                value.add_css_class("plume-asleep");
            } else {
                value.remove_css_class("plume-asleep");
            }
            card::set(&state, st);
            card::set(&util, &g.busy.map(format::percent).unwrap_or_else(|| "–".into()));
            match (g.freq_mhz, g.freq_max_mhz) {
                (Some(f), Some(m)) => card::set(&freq, &format!("{} / {}", format::mhz(f), format::mhz(m))),
                (Some(f), None) => card::set(&freq, &format::mhz(f)),
                (None, Some(m)) => card::set(&freq, &format!("– / {}", format::mhz(m))),
                _ => card::set(&freq, "–"),
            }
            if let Some(l) = &vram {
                match (g.mem_used, g.mem_total) {
                    (Some(u), Some(t)) if !asleep => card::set(l, &format!("{} of {}", format::bytes(u), format::bytes(t))),
                    (_, Some(t)) => card::set(l, &format!("– of {}", format::bytes(t))),
                    _ => card::set(l, "–"),
                }
            }
            if let Some(l) = &temp {
                card::set(l, &g.temp_c.map(|t| format!("{t:.0} °C")).unwrap_or_else(|| "–".into()));
            }
            if let Some(l) = &power {
                card::set(l, &g.power_w.map(|w| format!("{w:.1} W")).unwrap_or_else(|| "–".into()));
            }
        }));
        add(&c.root);
    }

    // ------------------------------------------------------------ Network
    {
        let c = card("Network", Some("net"));
        let g = Graph::new(
            &[
                Line { key: Key::NetRx, color: Color::Rx, fill: true },
                Line { key: Key::NetTx, color: Color::Tx, fill: false },
            ],
            Scale::Auto { floor: 16.0 * 1024.0 },
        );
        crate::app::zoomable(&g);
        let (frame, corner) = graph_frame(&g, 150);
        g.connect_ymax(move |m| card::set(&corner, &format::rate(m as f64)));
        c.body.append(&frame);
        let down = stat(&c, "Download", Some("plume-sw-rx"));
        let up = stat(&c, "Upload", Some("plume-sw-tx"));
        let rx_total = stat(&c, "Received", None);
        let tx_total = stat(&c, "Sent", None);
        let ifaces = stat(&c, "Interfaces", None);
        let (value, sub) = (c.value.clone(), c.sub.clone());
        updaters.push(Box::new(move |s| {
            let n = &s.net;
            let (rx, tx) = (avg(&s.hist.net_rx, n.rx_rate), avg(&s.hist.net_tx, n.tx_rate));
            card::set(&value, &format!("↓ {}  ↑ {}", format::rate(rx), format::rate(tx)));
            card::set(&down, &format::rate(rx));
            card::set(&up, &format::rate(tx));
            card::set(&rx_total, &format::bytes(n.rx_total));
            card::set(&tx_total, &format::bytes(n.tx_total));
            let names: Vec<String> = n
                .interfaces
                .iter()
                .filter(|i| i.physical)
                .map(|i| if i.wireless { format!("{} (Wi-Fi)", i.name) } else { i.name.clone() })
                .collect();
            let names = if names.is_empty() { "None".to_string() } else { names.join(", ") };
            card::set(&ifaces, &names);
            card::set(&sub, &names);
        }));
        add(&c.root);
    }

    // --------------------------------------------------------------- Disk
    {
        let c = card("Disk", Some("disk"));
        let g = Graph::new(
            &[
                Line { key: Key::DiskRead, color: Color::Read, fill: true },
                Line { key: Key::DiskWrite, color: Color::Write, fill: false },
            ],
            Scale::Auto { floor: 64.0 * 1024.0 },
        );
        crate::app::zoomable(&g);
        let (frame, corner) = graph_frame(&g, 150);
        g.connect_ymax(move |m| card::set(&corner, &format::rate(m as f64)));
        c.body.append(&frame);
        let read = stat(&c, "Read", Some("plume-sw-read"));
        let write = stat(&c, "Write", Some("plume-sw-write"));
        let busy = stat(&c, "Active time", None);
        let devs = stat(&c, "Devices", None);

        let fs_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        fs_box.set_margin_top(4);
        c.root.append(&fs_box);
        let fs_rows: Rc<RefCell<Vec<(String, gtk::LevelBar, gtk::Label)>>> = Rc::default();

        let (value, sub) = (c.value.clone(), c.sub.clone());
        updaters.push(Box::new(move |s| {
            let d = &s.disk;
            let (rd, wr) = (avg(&s.hist.disk_read, d.read_rate), avg(&s.hist.disk_write, d.write_rate));
            card::set(&value, &format!("R {}  W {}", format::rate(rd), format::rate(wr)));
            card::set(&read, &format::rate(rd));
            card::set(&write, &format::rate(wr));
            card::set(&busy, &format::percent(d.busy));
            let names: Vec<String> = d.disks.iter().map(|x| x.name.clone()).collect();
            card::set(&devs, &names.join(", "));
            let models: Vec<String> = d
                .disks
                .iter()
                .map(|x| if x.model.is_empty() { x.name.clone() } else { format!("{} ({})", x.model, format::bytes(x.size)) })
                .collect();
            card::set(&sub, &models.join(" · "));

            // Filesystems: rebuild rows only when the mount set changes.
            let mut rows = fs_rows.borrow_mut();
            let same = rows.len() == s.filesystems.len()
                && rows.iter().zip(&s.filesystems).all(|(r, f)| r.0 == f.mount);
            if !same {
                while let Some(ch) = fs_box.first_child() {
                    fs_box.remove(&ch);
                }
                rows.clear();
                for f in &s.filesystems {
                    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                    let name = gtk::Label::new(Some(&f.mount));
                    name.set_xalign(0.0);
                    name.set_width_chars(12);
                    name.set_max_width_chars(18);
                    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    name.set_tooltip_text(Some(&format!("{} ({})", f.device, f.fstype)));
                    let bar = gtk::LevelBar::new();
                    bar.set_hexpand(true);
                    bar.set_valign(gtk::Align::Center);
                    let lbl = gtk::Label::new(None);
                    lbl.add_css_class("plume-num");
                    lbl.add_css_class("plume-cap");
                    line.append(&name);
                    line.append(&bar);
                    line.append(&lbl);
                    fs_box.append(&line);
                    rows.push((f.mount.clone(), bar, lbl));
                }
            }
            for ((_, bar, lbl), f) in rows.iter().zip(&s.filesystems) {
                bar.set_value(f.used as f64 / f.total.max(1) as f64);
                card::set(lbl, &format!("{} free of {}", format::bytes(f.avail), format::bytes(f.total)));
            }
        }));
        add(&c.root);
    }

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_child(Some(&flow));

    // One timer for all labels, only while visible.
    let updaters = Rc::new(updaters);
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let run = {
        let updaters = updaters.clone();
        move || crate::app::with_shared(|s| updaters.iter().for_each(|u| u(s)))
    };
    let run = Rc::new(run);
    scroller.connect_map(glib::clone!(
        #[strong]
        timer,
        #[strong]
        run,
        move |_| {
            run();
            let run = run.clone();
            let id = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                run();
                glib::ControlFlow::Continue
            });
            if let Some(old) = timer.replace(Some(id)) {
                old.remove();
            }
        }
    ));
    scroller.connect_unmap(move |_| {
        if let Some(id) = timer.take() {
            id.remove();
        }
    });
    scroller.upcast()
}
