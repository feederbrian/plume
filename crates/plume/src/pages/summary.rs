//! Bottom summary panels for the process tabs (macOS Activity Monitor style):
//! stats on the left and right, a live graph in the middle.

use super::processes::Channel;
use crate::format;
use crate::widgets::card::{graph_frame, set};
use crate::widgets::{Color, Graph, Key, Line, Scale};
use gtk::prelude::*;
use plume_core::Shared;

type Updater = Box<dyn Fn(&Shared)>;

/// 1-second mean of a history series (falls back to the instant value).
fn avg(series: &plume_core::history::Series, instant: f64) -> f64 {
    series.mean_recent(1_000_000).map(|v| v as f64).unwrap_or(instant)
}

fn column() -> gtk::Grid {
    let g = gtk::Grid::new();
    g.set_row_spacing(4);
    g.set_column_spacing(14);
    g.set_valign(gtk::Align::Center);
    g
}

/// Adds "caption  value" to a stats grid and returns the value label.
fn kv(g: &gtk::Grid, row: i32, caption: &str, swatch: Option<&str>) -> gtk::Label {
    let cap_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if let Some(cls) = swatch {
        let sw = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        sw.add_css_class("plume-swatch");
        sw.add_css_class(cls);
        sw.set_valign(gtk::Align::Center);
        cap_box.append(&sw);
    }
    let cap = gtk::Label::new(Some(caption));
    cap.set_xalign(0.0);
    cap.add_css_class("plume-cap");
    cap_box.append(&cap);
    let v = gtk::Label::new(Some("–"));
    v.set_xalign(1.0);
    v.set_width_chars(10);
    v.add_css_class("plume-statval");
    g.attach(&cap_box, 0, row, 1, 1);
    g.attach(&v, 1, row, 1, 1);
    v
}

pub fn build(ch: Channel) -> (gtk::Widget, Updater) {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    root.add_css_class("plume-summary");
    root.set_vexpand(false);
    root.set_margin_top(10);
    root.set_margin_bottom(12);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let left = column();
    let right = column();
    let mid = gtk::Box::new(gtk::Orientation::Vertical, 4);
    mid.set_hexpand(true);
    let title = gtk::Label::new(None);
    title.set_xalign(0.0);
    title.add_css_class("plume-cap");
    title.add_css_class("plume-graph-title");
    mid.append(&title);

    let (lines, scale, graph_title): (Vec<Line>, Scale, &str) = match ch {
        Channel::Cpu => (
            vec![
                Line { key: Key::Cpu, color: Color::Cpu, fill: true },
                Line { key: Key::CpuKernel, color: Color::Kernel, fill: true },
            ],
            Scale::Unit,
            "CPU LOAD",
        ),
        Channel::Memory => (
            vec![
                Line { key: Key::Mem, color: Color::Mem, fill: true },
                Line { key: Key::Swap, color: Color::Swap, fill: false },
            ],
            Scale::Unit,
            "MEMORY USED",
        ),
        Channel::Disk => (
            vec![
                Line { key: Key::DiskRead, color: Color::Read, fill: true },
                Line { key: Key::DiskWrite, color: Color::Write, fill: false },
            ],
            Scale::Auto { floor: 64.0 * 1024.0 },
            "DATA READ / WRITTEN",
        ),
        Channel::Network => (
            vec![
                Line { key: Key::NetRx, color: Color::Rx, fill: true },
                Line { key: Key::NetTx, color: Color::Tx, fill: false },
            ],
            Scale::Auto { floor: 16.0 * 1024.0 },
            "DATA RECEIVED / SENT",
        ),
    };
    title.set_text(graph_title);
    let graph = Graph::new(&lines, scale);
    crate::app::zoomable(&graph);
    graph.set_vexpand(false);
    let (frame, corner) = graph_frame(&graph, 96);
    frame.set_vexpand(false);
    if matches!(scale, Scale::Auto { .. }) {
        graph.connect_ymax(move |m| set(&corner, &format::rate(m as f64)));
    }
    mid.append(&frame);

    root.append(&left);
    root.append(&mid);
    root.append(&right);

    let up: Updater = match ch {
        Channel::Cpu => {
            let sys = kv(&left, 0, "System", Some("plume-sw-kernel"));
            let user = kv(&left, 1, "User", Some("plume-sw-cpu"));
            let idle = kv(&left, 2, "Idle", None);
            let pressure = kv(&left, 3, "Pressure", None);
            pressure.set_tooltip_text(Some("Share of the last 10 s in which runnable tasks waited for a CPU (PSI)"));
            let procs = kv(&right, 0, "Processes", None);
            let threads = kv(&right, 1, "Threads", None);
            let load = kv(&right, 2, "Load average", None);
            let speed = kv(&right, 3, "Speed", None);
            Box::new(move |s| {
                let total = avg(&s.hist.cpu, s.cpu.total as f64) as f32;
                let k = (avg(&s.hist.cpu_kernel, s.cpu.kernel as f64) as f32).min(total);
                set(&sys, &format::percent(k));
                set(&user, &format::percent(total - k));
                set(&idle, &format::percent(1.0 - total));
                set(&pressure, &format!("{:.1} %", s.psi[0]));
                set(&procs, &s.n_procs.to_string());
                set(&threads, &s.n_threads.to_string());
                set(&load, &format!("{:.2} {:.2} {:.2}", s.load[0], s.load[1], s.load[2]));
                let f: Vec<u32> = s.cpu_freq.iter().copied().filter(|&m| m > 0).collect();
                if !f.is_empty() {
                    let avg = f.iter().map(|&m| m as u64).sum::<u64>() / f.len() as u64;
                    set(&speed, &format::mhz(avg as u32));
                }
            })
        }
        Channel::Memory => {
            let phys = kv(&left, 0, "Physical memory", None);
            let used = kv(&left, 1, "Memory used", Some("plume-sw-mem"));
            let cache = kv(&left, 2, "Cached files", None);
            let avail = kv(&left, 3, "Available", None);
            let swap = kv(&right, 0, "Swap used", Some("plume-sw-swap"));
            let shared = kv(&right, 1, "Shared", None);
            let dirty = kv(&right, 2, "Waiting to write", None);
            let pressure = kv(&right, 3, "Pressure", None);
            pressure.set_tooltip_text(Some(
                "Share of the last 10 s in which tasks stalled waiting for memory (PSI). \
                 The Linux counterpart of macOS memory pressure.",
            ));
            Box::new(move |s| {
                let m = &s.mem;
                set(&phys, &format::bytes(m.total));
                set(&used, &format::bytes(m.used));
                set(&cache, &format::bytes(m.cache));
                set(&avail, &format::bytes(m.available));
                set(&swap, &if m.swap_total > 0 { format!("{} / {}", format::bytes(m.swap_used), format::bytes(m.swap_total)) } else { "None".into() });
                set(&shared, &format::bytes(m.shmem));
                set(&dirty, &format::bytes(m.dirty));
                set(&pressure, &format!("{:.1} %", s.psi[1]));
            })
        }
        Channel::Disk => {
            let riops = kv(&left, 0, "Reads in/s", None);
            let wiops = kv(&left, 1, "Writes out/s", None);
            let rrate = kv(&left, 2, "Data read/s", Some("plume-sw-read"));
            let wrate = kv(&left, 3, "Data written/s", Some("plume-sw-write"));
            let rtot = kv(&right, 0, "Data read", None);
            let wtot = kv(&right, 1, "Data written", None);
            let busy = kv(&right, 2, "Active time", None);
            let pressure = kv(&right, 3, "I/O pressure", None);
            rtot.set_tooltip_text(Some("Since boot"));
            wtot.set_tooltip_text(Some("Since boot"));
            Box::new(move |s| {
                let d = &s.disk;
                set(&riops, &format!("{:.0}", d.read_iops));
                set(&wiops, &format!("{:.0}", d.write_iops));
                set(&rrate, &format::rate(avg(&s.hist.disk_read, d.read_rate)));
                set(&wrate, &format::rate(avg(&s.hist.disk_write, d.write_rate)));
                set(&rtot, &format::bytes(d.read_bytes));
                set(&wtot, &format::bytes(d.written_bytes));
                set(&busy, &format::percent(d.busy));
                set(&pressure, &format!("{:.1} %", s.psi[2]));
            })
        }
        Channel::Network => {
            let pin = kv(&left, 0, "Packets in/s", None);
            let pout = kv(&left, 1, "Packets out/s", None);
            let rin = kv(&left, 2, "Data received/s", Some("plume-sw-rx"));
            let rout = kv(&left, 3, "Data sent/s", Some("plume-sw-tx"));
            let tin = kv(&right, 0, "Data received", None);
            let tout = kv(&right, 1, "Data sent", None);
            let ifs = kv(&right, 2, "Interfaces", None);
            let scope = kv(&right, 3, "Per-process", None);
            let root_user = unsafe { libc::geteuid() } == 0;
            set(&scope, if root_user { "TCP, all users" } else { "TCP, your processes" });
            scope.set_tooltip_text(Some(
                "Per-process figures come from the kernel's TCP connection counters, \
                 mapped to processes through /proc. UDP traffic (including QUIC / HTTP3 \
                 used by browsers) is not attributed, and other users' processes need root. \
                 Interface totals on the left are complete.",
            ));
            Box::new(move |s| {
                let n = &s.net;
                set(&pin, &format!("{:.0}", n.rx_pps));
                set(&pout, &format!("{:.0}", n.tx_pps));
                set(&rin, &format::rate(avg(&s.hist.net_rx, n.rx_rate)));
                set(&rout, &format::rate(avg(&s.hist.net_tx, n.tx_rate)));
                set(&tin, &format::bytes(n.rx_total));
                set(&tout, &format::bytes(n.tx_total));
                let names: Vec<&str> = n.interfaces.iter().filter(|i| i.physical).map(|i| i.name.as_str()).collect();
                set(&ifs, &if names.is_empty() { "None".into() } else { names.join(", ") });
            })
        }
    };
    (root.upcast(), up)
}
