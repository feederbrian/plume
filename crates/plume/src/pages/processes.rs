//! Per-resource process tabs (CPU / Memory / Disk / Network), macOS style:
//! each tab is the process list with resource-specific columns and a live
//! summary panel at the bottom. Search, filter and selection are shared.
//!
//! "Slot" model: a list holds N stable row objects; row *i* shows whatever
//! process is currently *i*-th in sort order. Each refresh rewrites slot
//! contents in place and only bound (visible) cells whose text changed are
//! touched. This keeps the scroll position put, never rebuilds the list, and
//! costs O(visible cells) widget work. The selection follows the *process*.

use crate::{app, format, settings};
use adw::prelude::*;
use gtk::{gdk, gio, glib, subclass::prelude::*};
use plume_core::actions::{self, ActionError, Sig};
use plume_core::proc::ProcInfo;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::rc::{Rc, Weak};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Cpu,
    Memory,
    Disk,
    Network,
}

impl Channel {
    pub const ALL: [Channel; 4] = [Channel::Cpu, Channel::Memory, Channel::Disk, Channel::Network];

    pub fn id(self) -> &'static str {
        match self {
            Channel::Cpu => "cpu",
            Channel::Memory => "memory",
            Channel::Disk => "disk",
            Channel::Network => "network",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Channel::Cpu => "CPU",
            Channel::Memory => "Memory",
            Channel::Disk => "Disk",
            Channel::Network => "Network",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Channel::Cpu => "computer-symbolic",
            Channel::Memory => "drive-harddisk-solidstate-symbolic",
            Channel::Disk => "drive-harddisk-symbolic",
            Channel::Network => "network-transmit-receive-symbolic",
        }
    }
    pub fn from_id(id: &str) -> Option<Channel> {
        Channel::ALL.iter().copied().find(|c| c.id() == id)
    }
    fn columns(self) -> &'static [Col] {
        use Col::*;
        match self {
            Channel::Cpu => &[Name, Cpu, CpuTime, Threads, Nice, State, Pid, User, Command],
            Channel::Memory => &[Name, Memory, Resident, Shared, Virtual, Threads, Pid, User, Command],
            Channel::Disk => &[Name, Write, Read, WriteTotal, ReadTotal, Pid, User, Command],
            Channel::Network => &[Name, NetTx, NetRx, NetTxTotal, NetRxTotal, Conns, Pid, User, Command],
        }
    }
    fn default_sort(self) -> Col {
        match self {
            Channel::Cpu => Col::Cpu,
            Channel::Memory => Col::Memory,
            Channel::Disk => Col::Write,
            Channel::Network => Col::NetRx,
        }
    }
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Row {
    pub pid: i32,
    pub start: u64,
    pub name: Arc<str>,
    pub user: Arc<str>,
    pub cpu: f32,
    pub cpu_ticks: u64,
    pub memory: u64,
    pub rss: u64,
    pub shared: u64,
    pub vsize: u64,
    pub read: Option<f32>,
    pub write: Option<f32>,
    pub read_total: u64,
    pub write_total: u64,
    pub net: Option<plume_core::netproc::ProcNet>,
    pub threads: u32,
    pub nice: i8,
    pub state: u8,
    pub cmdline: Arc<str>,
    /// CPU divisor (core count or 1); part of the row so a settings change re-renders.
    pub div: f32,
}

mod imp {
    use super::*;
    use std::sync::OnceLock;

    #[derive(Default)]
    pub struct ProcSlot {
        pub row: RefCell<Row>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProcSlot {
        const NAME: &'static str = "PlumeProcSlot";
        type Type = super::ProcSlot;
    }

    impl ObjectImpl for ProcSlot {
        fn signals() -> &'static [glib::subclass::Signal] {
            static S: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            S.get_or_init(|| vec![glib::subclass::Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    pub struct ProcSlot(ObjectSubclass<imp::ProcSlot>);
}

impl ProcSlot {
    fn new() -> ProcSlot {
        glib::Object::new()
    }
    pub fn row(&self) -> std::cell::Ref<'_, Row> {
        self.imp().row.borrow()
    }
    fn set(&self, r: Row) {
        let changed = *self.imp().row.borrow() != r;
        if changed {
            self.imp().row.replace(r);
            self.emit_by_name::<()>("changed", &[]);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Name,
    User,
    Cpu,
    CpuTime,
    Memory,
    Resident,
    Shared,
    Virtual,
    Read,
    Write,
    ReadTotal,
    WriteTotal,
    NetRx,
    NetTx,
    NetRxTotal,
    NetTxTotal,
    Conns,
    Pid,
    Threads,
    Nice,
    State,
    Command,
}

const ALL_COLS: [Col; 22] = [
    Col::Name,
    Col::User,
    Col::Cpu,
    Col::CpuTime,
    Col::Memory,
    Col::Resident,
    Col::Shared,
    Col::Virtual,
    Col::Read,
    Col::Write,
    Col::ReadTotal,
    Col::WriteTotal,
    Col::NetRx,
    Col::NetTx,
    Col::NetRxTotal,
    Col::NetTxTotal,
    Col::Conns,
    Col::Pid,
    Col::Threads,
    Col::Nice,
    Col::State,
    Col::Command,
];

impl Col {
    fn id(self) -> &'static str {
        match self {
            Col::Name => "name",
            Col::User => "user",
            Col::Cpu => "cpu",
            Col::CpuTime => "cputime",
            Col::Memory => "memory",
            Col::Resident => "rss",
            Col::Shared => "shared",
            Col::Virtual => "virtual",
            Col::Read => "read",
            Col::Write => "write",
            Col::ReadTotal => "readtotal",
            Col::WriteTotal => "writetotal",
            Col::NetRx => "netrx",
            Col::NetTx => "nettx",
            Col::NetRxTotal => "netrxtotal",
            Col::NetTxTotal => "nettxtotal",
            Col::Conns => "conns",
            Col::Pid => "pid",
            Col::Threads => "threads",
            Col::Nice => "nice",
            Col::State => "state",
            Col::Command => "command",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Col::Name => "Process Name",
            Col::User => "User",
            Col::Cpu => "% CPU",
            Col::CpuTime => "CPU Time",
            Col::Memory => "Memory",
            Col::Resident => "Resident",
            Col::Shared => "Shared",
            Col::Virtual => "Virtual",
            Col::Read => "Read/s",
            Col::Write => "Write/s",
            Col::ReadTotal => "Bytes Read",
            Col::WriteTotal => "Bytes Written",
            Col::NetRx => "Rcvd/s",
            Col::NetTx => "Sent/s",
            Col::NetRxTotal => "Rcvd Bytes",
            Col::NetTxTotal => "Sent Bytes",
            Col::Conns => "Connections",
            Col::Pid => "PID",
            Col::Threads => "Threads",
            Col::Nice => "Nice",
            Col::State => "Status",
            Col::Command => "Command Line",
        }
    }
    fn width(self) -> i32 {
        match self {
            Col::Name => 230,
            Col::User => 90,
            Col::Cpu | Col::Pid | Col::Threads => 75,
            Col::Nice => 55,
            Col::State => 85,
            Col::Command => 300,
            Col::Conns => 100,
            _ => 105,
        }
    }
    fn numeric(self) -> bool {
        !matches!(self, Col::Name | Col::User | Col::State | Col::Command)
    }
    fn text(self, r: &Row) -> String {
        let net = |f: fn(&plume_core::netproc::ProcNet) -> String| r.net.as_ref().map(f).unwrap_or_else(|| "–".into());
        match self {
            Col::Name => r.name.to_string(),
            Col::User => r.user.to_string(),
            Col::Cpu => {
                let v = r.cpu * 100.0 / r.div;
                if v < 0.05 { "0".into() } else if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") }
            }
            Col::CpuTime => format::cpu_time(r.cpu_ticks, app::clock_ticks()),
            Col::Memory => if r.memory == 0 { "–".into() } else { format::bytes(r.memory) },
            Col::Resident => format::bytes(r.rss),
            Col::Shared => format::bytes(r.shared),
            Col::Virtual => format::bytes(r.vsize),
            Col::Read => rate(r.read),
            Col::Write => rate(r.write),
            Col::ReadTotal => if r.read.is_some() { format::bytes(r.read_total) } else { "–".into() },
            Col::WriteTotal => if r.write.is_some() { format::bytes(r.write_total) } else { "–".into() },
            Col::NetRx => net(|n| rate(Some(n.rx_rate))),
            Col::NetTx => net(|n| rate(Some(n.tx_rate))),
            Col::NetRxTotal => net(|n| format::bytes(n.rx_total)),
            Col::NetTxTotal => net(|n| format::bytes(n.tx_total)),
            Col::Conns => net(|n| n.connections.to_string()),
            Col::Pid => r.pid.to_string(),
            Col::Threads => r.threads.to_string(),
            Col::Nice => r.nice.to_string(),
            Col::State => format::state(r.state).to_string(),
            Col::Command => r.cmdline.to_string(),
        }
    }
    fn cmp(self, a: &ProcInfo, b: &ProcInfo) -> Ordering {
        let f = |o: Option<f32>| o.unwrap_or(-1.0);
        let n = |p: &ProcInfo, g: fn(&plume_core::netproc::ProcNet) -> f64| p.net.as_ref().map(g).unwrap_or(-1.0);
        let o = match self {
            Col::Name => cmp_ci(&a.name, &b.name),
            Col::User => a.user.cmp(&b.user),
            Col::Cpu => a.cpu.total_cmp(&b.cpu),
            Col::CpuTime => a.cpu_ticks.cmp(&b.cpu_ticks),
            Col::Memory => a.memory.cmp(&b.memory),
            Col::Resident => a.rss.cmp(&b.rss),
            Col::Shared => a.shared.cmp(&b.shared),
            Col::Virtual => a.vsize.cmp(&b.vsize),
            Col::Read => f(a.read_rate).total_cmp(&f(b.read_rate)),
            Col::Write => f(a.write_rate).total_cmp(&f(b.write_rate)),
            Col::ReadTotal => a.read_total.cmp(&b.read_total),
            Col::WriteTotal => a.write_total.cmp(&b.write_total),
            Col::NetRx => n(a, |x| x.rx_rate as f64).total_cmp(&n(b, |x| x.rx_rate as f64)),
            Col::NetTx => n(a, |x| x.tx_rate as f64).total_cmp(&n(b, |x| x.tx_rate as f64)),
            Col::NetRxTotal => n(a, |x| x.rx_total as f64).total_cmp(&n(b, |x| x.rx_total as f64)),
            Col::NetTxTotal => n(a, |x| x.tx_total as f64).total_cmp(&n(b, |x| x.tx_total as f64)),
            Col::Conns => n(a, |x| x.connections as f64).total_cmp(&n(b, |x| x.connections as f64)),
            Col::Pid => a.pid.cmp(&b.pid),
            Col::Threads => a.threads.cmp(&b.threads),
            Col::Nice => a.nice.cmp(&b.nice),
            Col::State => a.state.cmp(&b.state),
            Col::Command => a.cmdline.cmp(&b.cmdline),
        };
        // Tie-break on CPU, then PID, so idle rows don't shuffle.
        o.then_with(|| b.cpu.total_cmp(&a.cpu)).then(a.pid.cmp(&b.pid))
    }
    fn from_id(id: &str) -> Option<Col> {
        ALL_COLS.iter().copied().find(|c| c.id() == id)
    }
}

fn rate(r: Option<f32>) -> String {
    match r {
        None => "–".into(),
        Some(v) if v < 1.0 => "0".into(),
        Some(v) => format::rate(v as f64),
    }
}

fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

// ---------------------------------------------------------------------------
// Shared state across tabs
// ---------------------------------------------------------------------------

#[derive(Default)]
struct State {
    selected: Cell<Option<(i32, u64)>>,
    query: RefCell<String>,
    filter: Cell<u32>,
    tables: RefCell<Vec<Weak<Table>>>,
    end_btn: RefCell<Option<gtk::Button>>,
}

thread_local! {
    static STATE: Rc<State> = Rc::new(State::default());
}

fn state() -> Rc<State> {
    STATE.with(Rc::clone)
}

fn refresh_all(force: bool) {
    let tables: Vec<Rc<Table>> = state().tables.borrow().iter().filter_map(Weak::upgrade).collect();
    for t in tables {
        if t.view.is_mapped() {
            refresh(&t, force);
        }
    }
}

fn update_end_button() {
    let s = state();
    let has = s.selected.get().is_some();
    let btn = s.end_btn.borrow().clone();
    if let Some(b) = btn {
        b.set_sensitive(has);
    }
}

/// Search entry, filter and End button; shown above the process tabs.
pub fn toolbar() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.set_margin_top(8);
    bar.set_margin_bottom(8);
    bar.set_margin_start(12);
    bar.set_margin_end(12);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search by name, user, PID or command"));
    search.set_hexpand(true);
    let filter = gtk::DropDown::from_strings(&["All Processes", "My Processes", "Active Processes"]);
    filter.set_tooltip_text(Some("Which processes to show"));
    let end_btn = gtk::Button::with_label("End Process");
    end_btn.add_css_class("destructive-action");
    end_btn.set_action_name(Some("proc.end"));
    end_btn.set_tooltip_text(Some("End the selected process (Delete)"));
    bar.append(&search);
    bar.append(&filter);
    bar.append(&end_btn);

    let s = state();
    let idx = settings::with(|st| match st.proc_filter.as_str() {
        "mine" => 1,
        "active" => 2,
        _ => 0,
    });
    filter.set_selected(idx);
    s.filter.set(idx);
    s.end_btn.replace(Some(end_btn));
    search.connect_search_changed(|e| {
        state().query.replace(e.text().to_lowercase());
        refresh_all(true);
    });
    filter.connect_selected_notify(|d| {
        state().filter.set(d.selected());
        let v = match d.selected() {
            1 => "mine",
            2 => "active",
            _ => "all",
        };
        settings::update(|s| s.proc_filter = v.into());
        refresh_all(true);
    });
    unsafe { bar.set_data("plume-search", search) };
    update_end_button();
    bar
}

pub fn focus_search(toolbar: &gtk::Box) {
    if let Some(s) = unsafe { toolbar.data::<gtk::SearchEntry>("plume-search") } {
        unsafe { s.as_ref() }.grab_focus();
    }
}

/// Re-renders after a settings change (CPU divisor, kernel threads...).
pub fn reload() {
    refresh_all(true);
}

/// Process actions ("proc.end" etc.), acting on the shared selection.
pub fn actions() -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let add = |name: &str, f: fn(Option<&glib::Variant>), ty: Option<&glib::VariantTy>| {
        let a = gio::SimpleAction::new(name, ty);
        a.connect_activate(move |_, p| f(p));
        group.add_action(&a);
    };
    add("end", |_| signal_selected(Sig::Term), None);
    add("kill", |_| signal_selected(Sig::Kill), None);
    add("stop", |_| signal_selected(Sig::Stop), None);
    add("cont", |_| signal_selected(Sig::Cont), None);
    add(
        "inspect",
        |_| {
            if let Some((pid, start)) = state().selected.get() {
                crate::inspector::open(pid, start);
            }
        },
        None,
    );
    add(
        "nice",
        |p| {
            let Some(n) = p.and_then(|v| v.get::<i32>()) else { return };
            if let Some((pid, start)) = state().selected.get() {
                renice(pid, start, n);
            }
        },
        Some(glib::VariantTy::INT32),
    );
    group
}

// ---------------------------------------------------------------------------
// One tab's table
// ---------------------------------------------------------------------------

struct Table {
    channel: Channel,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    view: gtk::ColumnView,
    menu: gtk::PopoverMenu,
    updating: Cell<bool>,
    last_gen: Cell<u64>,
    sort: Cell<(Col, bool)>,
}

/// Builds a full tab: process table + summary panel.
pub fn page(channel: Channel) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let store = gio::ListStore::new::<ProcSlot>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("plume-procs");
    view.set_show_column_separators(false);
    view.set_reorderable(true);
    view.set_vexpand(true);

    let mut columns = Vec::new();
    for &col in channel.columns() {
        let c = gtk::ColumnViewColumn::new(Some(col.title()), Some(cell_factory(col)));
        c.set_id(Some(col.id()));
        c.set_resizable(true);
        c.set_fixed_width(col.width());
        if col == Col::Command {
            c.set_expand(true);
        }
        // A sorter makes the header clickable; we sort ourselves (see refresh).
        c.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
        view.append_column(&c);
        columns.push((col, c));
    }
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&view));
    scroller.set_vexpand(true);
    root.append(&scroller);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let (summary, update_summary) = crate::pages::summary::build(channel);
    root.append(&summary);

    let menu = gtk::PopoverMenu::from_model(Some(&context_menu()));
    menu.set_parent(&view);
    menu.set_has_arrow(false);
    menu.set_halign(gtk::Align::Start);

    let (sc, sd) = settings::with(|s| {
        s.sorts
            .get(channel.id())
            .and_then(|v| {
                let (c, d) = v.split_once(':')?;
                Some((Col::from_id(c)?, d == "desc"))
            })
            .unwrap_or((channel.default_sort(), true))
    });
    let t = Rc::new(Table {
        channel,
        store,
        selection,
        view,
        menu,
        updating: Cell::new(false),
        last_gen: Cell::new(u64::MAX),
        sort: Cell::new((sc, sd)),
    });
    state().tables.borrow_mut().push(Rc::downgrade(&t));
    wire(&t, &columns);

    // Poll for new data only while this tab is visible.
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let weak = Rc::downgrade(&t);
    let t2 = timer.clone();
    let update_summary = Rc::new(update_summary);
    root.connect_map(move |_| {
        let weak = weak.clone();
        let us = update_summary.clone();
        // Never mutate the list model from inside map/size-allocate: GTK's list
        // widgets assert on items-changed during layout. Defer to idle.
        {
            let weak = weak.clone();
            let us = us.clone();
            glib::idle_add_local_once(move || {
                if let Some(t) = weak.upgrade() {
                    refresh(&t, true);
                }
                app::with_shared(|s| us(s));
            });
        }
        let mut n = 0u32;
        let id = glib::timeout_add_local(std::time::Duration::from_millis(125), move || {
            if let Some(t) = weak.upgrade() {
                refresh(&t, false);
            }
            n = n.wrapping_add(1);
            if n % 2 == 0 {
                app::with_shared(|s| us(s));
            }
            glib::ControlFlow::Continue
        });
        if let Some(old) = t2.replace(Some(id)) {
            old.remove();
        }
    });
    root.connect_unmap(move |_| {
        if let Some(id) = timer.take() {
            id.remove();
        }
    });
    // Keep the Table alive as long as the page.
    unsafe { root.set_data("plume-table", t) };
    root
}

fn cell_factory(col: Col) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, obj| {
        let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
        let l = gtk::Label::new(None);
        l.add_css_class("plume-cell");
        l.set_xalign(if col.numeric() { 1.0 } else { 0.0 });
        l.set_ellipsize(if col == Col::Command {
            gtk::pango::EllipsizeMode::End
        } else {
            gtk::pango::EllipsizeMode::Middle
        });
        if col.numeric() {
            l.add_css_class("plume-num");
        }
        item.set_child(Some(&l));
    });
    factory.connect_bind(move |_, obj| {
        let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else { return };
        let Some(slot) = item.item().and_downcast::<ProcSlot>() else { return };
        render(&label, col, &slot);
        stripe(&label, item.position());
        let weak = label.downgrade();
        let id = slot.connect_local("changed", false, move |args| {
            if let (Some(l), Ok(s)) = (weak.upgrade(), args[0].get::<ProcSlot>()) {
                render(&l, col, &s);
            }
            None
        });
        unsafe { label.set_data("plume-slot", (slot.downgrade(), id)) };
    });
    factory.connect_unbind(|_, obj| {
        let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else { return };
        let data: Option<(glib::WeakRef<ProcSlot>, glib::SignalHandlerId)> = unsafe { label.steal_data("plume-slot") };
        if let Some((slot, id)) = data {
            if let Some(s) = slot.upgrade() {
                s.disconnect(id);
            }
        }
    });
    factory
}

/// Zebra striping. Row widgets are recycled while scrolling, so CSS
/// :nth-child would flip parity; instead tag by *item position*, which is
/// stable in the slot model (slots are only appended/removed at the end).
/// The class goes on our own cell label, which fills the whole cell; touching
/// GTK's row widget during bind upsets GtkListBase (seen as an assertion in
/// gtk_list_base_update_adjustments on GTK 4.14).
fn stripe(label: &gtk::Label, pos: u32) {
    if pos % 2 == 1 {
        label.add_css_class("plume-odd");
    } else {
        label.remove_css_class("plume-odd");
    }
}

fn context_menu() -> gio::Menu {
    let m = gio::Menu::new();
    let s1 = gio::Menu::new();
    s1.append(Some("Properties"), Some("proc.inspect"));
    m.append_section(None, &s1);
    let s2 = gio::Menu::new();
    s2.append(Some("Stop"), Some("proc.stop"));
    s2.append(Some("Continue"), Some("proc.cont"));
    s2.append(Some("End"), Some("proc.end"));
    s2.append(Some("Kill"), Some("proc.kill"));
    m.append_section(None, &s2);
    let pri = gio::Menu::new();
    for (label, n) in [("Very High", -20), ("High", -5), ("Normal", 0), ("Low", 5), ("Very Low", 19)] {
        let it = gio::MenuItem::new(Some(label), None);
        it.set_action_and_target_value(Some("proc.nice"), Some(&n.to_variant()));
        pri.append_item(&it);
    }
    let s3 = gio::Menu::new();
    s3.append_submenu(Some("Change Priority"), &pri);
    m.append_section(None, &s3);
    m
}

fn wire(t: &Rc<Table>, columns: &[(Col, gtk::ColumnViewColumn)]) {
    let (sc, sd) = t.sort.get();
    if let Some((_, c)) = columns.iter().find(|(c, _)| *c == sc) {
        t.view.sort_by_column(Some(c), if sd { gtk::SortType::Descending } else { gtk::SortType::Ascending });
    }
    if let Some(sorter) = t.view.sorter().and_downcast::<gtk::ColumnViewSorter>() {
        let weak = Rc::downgrade(t);
        sorter.connect_changed(move |s, _| {
            let Some(t) = weak.upgrade() else { return };
            let col = s
                .primary_sort_column()
                .and_then(|c| c.id())
                .and_then(|id| Col::from_id(&id))
                .unwrap_or(t.channel.default_sort());
            let desc = s.primary_sort_order() == gtk::SortType::Descending;
            t.sort.set((col, desc));
            let ch = t.channel.id();
            settings::update(|st| {
                st.sorts.insert(ch.into(), format!("{}:{}", col.id(), if desc { "desc" } else { "asc" }));
            });
            refresh(&t, true);
        });
    }

    let weak = Rc::downgrade(t);
    t.selection.connect_selected_notify(move |sel| {
        let Some(t) = weak.upgrade() else { return };
        if t.updating.get() {
            return;
        }
        let item = sel.selected_item().and_downcast::<ProcSlot>();
        state().selected.set(item.map(|s| (s.row().pid, s.row().start)));
        update_end_button();
    });

    // Double-click / Enter → properties.
    let weak = Rc::downgrade(t);
    t.view.connect_activate(move |_, pos| {
        let Some(t) = weak.upgrade() else { return };
        if let Some(slot) = t.store.item(pos).and_downcast::<ProcSlot>() {
            let (pid, start) = (slot.row().pid, slot.row().start);
            crate::inspector::open(pid, start);
        }
    });

    // Right-click → select the row under the pointer and show the menu.
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    let weak = Rc::downgrade(t);
    click.connect_pressed(move |g, _, x, y| {
        let Some(t) = weak.upgrade() else { return };
        let Some(slot) = slot_at(&t.view, x, y) else { return };
        if let Some(pos) = t.store.find(&slot) {
            t.selection.set_selected(pos);
        }
        t.menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        t.menu.popup();
        g.set_state(gtk::EventSequenceState::Claimed);
    });
    t.view.add_controller(click);

    let sc = gtk::ShortcutController::new();
    for (accel, action) in [
        ("Delete", "proc.end"),
        ("<Shift>Delete", "proc.kill"),
        ("<Control>i", "proc.inspect"),
        ("<Alt>Return", "proc.inspect"),
        ("<Control>s", "proc.stop"),
        ("<Control>c", "proc.cont"),
    ] {
        sc.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(accel),
            Some(gtk::NamedAction::new(action)),
        ));
    }
    t.view.add_controller(sc);
}

fn render(label: &gtk::Label, col: Col, slot: &ProcSlot) {
    let r = slot.row();
    let text = col.text(&r);
    if label.text() != text {
        label.set_text(&text);
    }
    if col == Col::Command || col == Col::Name {
        label.set_tooltip_text(if r.cmdline.is_empty() { None } else { Some(&r.cmdline) });
    }
}

fn slot_at(view: &gtk::ColumnView, x: f64, y: f64) -> Option<ProcSlot> {
    let mut w = view.pick(x, y, gtk::PickFlags::DEFAULT)?;
    loop {
        let label = w.downcast_ref::<gtk::Label>().cloned().or_else(|| w.first_child().and_downcast::<gtk::Label>());
        if let Some(l) = label {
            let data = unsafe { l.data::<(glib::WeakRef<ProcSlot>, glib::SignalHandlerId)>("plume-slot") };
            if let Some(d) = data {
                return unsafe { d.as_ref() }.0.upgrade();
            }
        }
        if w == *view.upcast_ref::<gtk::Widget>() {
            return None;
        }
        w = w.parent()?;
    }
}

fn refresh(t: &Rc<Table>, force: bool) {
    let (procs, gen, ncpu) = app::with_shared(|s| (s.procs.clone(), s.procs_gen, s.info.cpu.logical));
    if !force && gen == t.last_gen.get() {
        return;
    }
    t.last_gen.set(gen);
    let st = state();
    let (divide, kthreads) = settings::with(|s| (s.divide_cpu, s.show_kernel_threads));
    let div = if divide { ncpu.max(1) as f32 } else { 1.0 };
    let uid = unsafe { libc::getuid() };
    let q = st.query.borrow().clone();
    let qpid: Option<i32> = q.parse().ok();
    let filter = st.filter.get();
    let channel = t.channel;

    let mut v: Vec<&ProcInfo> = procs
        .iter()
        .filter(|p| kthreads || !p.kernel_thread)
        .filter(|p| match filter {
            1 => p.uid == uid,
            2 => match channel {
                Channel::Disk => p.read_rate.unwrap_or(0.0) + p.write_rate.unwrap_or(0.0) > 0.0,
                Channel::Network => p.net.is_some_and(|n| n.rx_rate + n.tx_rate > 0.0),
                _ => p.cpu > 0.001 || p.state == b'R',
            },
            _ => true,
        })
        .filter(|p| {
            q.is_empty()
                || qpid == Some(p.pid)
                || p.name.to_lowercase().contains(&q)
                || p.user.to_lowercase().contains(&q)
                || p.cmdline.to_lowercase().contains(&q)
        })
        .collect();
    let (col, desc) = t.sort.get();
    v.sort_unstable_by(|a, b| {
        let o = col.cmp(a, b);
        if desc { o.reverse() } else { o }
    });

    let n_old = t.store.n_items() as usize;
    let n_new = v.len();
    t.updating.set(true);
    if n_new > n_old {
        let add: Vec<ProcSlot> = (n_old..n_new).map(|_| ProcSlot::new()).collect();
        for (slot, p) in add.iter().zip(&v[n_old..]) {
            slot.set(row_of(p, div));
        }
        t.store.extend_from_slice(&add);
    } else if n_new < n_old {
        t.store.splice(n_new as u32, (n_old - n_new) as u32, &[] as &[ProcSlot]);
    }
    for (k, p) in v.iter().enumerate().take(n_old.min(n_new)) {
        if let Some(slot) = t.store.item(k as u32).and_downcast::<ProcSlot>() {
            slot.set(row_of(p, div));
        }
    }

    // Keep the (shared) selection on the same process.
    if let Some((pid, start)) = st.selected.get() {
        match v.iter().position(|p| p.pid == pid && p.start_ticks == start) {
            Some(k) if t.selection.selected() != k as u32 => t.selection.set_selected(k as u32),
            Some(_) => {}
            None => {
                t.selection.set_selected(gtk::INVALID_LIST_POSITION);
                if !procs.iter().any(|p| p.pid == pid && p.start_ticks == start) {
                    st.selected.set(None); // exited
                }
            }
        }
    } else if t.selection.selected() != gtk::INVALID_LIST_POSITION {
        t.selection.set_selected(gtk::INVALID_LIST_POSITION);
    }
    t.updating.set(false);
    update_end_button();
}

fn row_of(p: &ProcInfo, div: f32) -> Row {
    Row {
        pid: p.pid,
        start: p.start_ticks,
        name: p.name.clone(),
        user: p.user.clone(),
        cpu: p.cpu,
        cpu_ticks: p.cpu_ticks,
        memory: p.memory,
        rss: p.rss,
        shared: p.shared,
        vsize: p.vsize,
        read: p.read_rate,
        write: p.write_rate,
        read_total: p.read_total,
        write_total: p.write_total,
        net: p.net,
        threads: p.threads,
        nice: p.nice,
        state: p.state,
        cmdline: p.cmdline.clone(),
        div,
    }
}

fn selected_proc() -> Option<(i32, u64, String)> {
    let (pid, start) = state().selected.get()?;
    let name = app::with_shared(|s| {
        s.procs.iter().find(|p| p.pid == pid && p.start_ticks == start).map(|p| p.name.to_string())
    })?;
    Some((pid, start, name))
}

fn signal_selected(sig: Sig) {
    let Some((pid, start, name)) = selected_proc() else { return };
    request_signal(pid, start, &name, sig);
}

/// Confirms (for End/Kill), sends the signal, and offers admin escalation on EPERM.
pub fn request_signal(pid: i32, start: u64, name: &str, sig: Sig) {
    let needs_confirm = matches!(sig, Sig::Term | Sig::Kill) && settings::with(|s| s.confirm_kill);
    if !needs_confirm {
        do_signal(pid, start, name.to_string(), sig);
        return;
    }
    let (heading, verb, body) = match sig {
        Sig::Kill => (
            format!("Kill “{name}”?"),
            "Kill Process",
            "Killing a process stops it immediately. Unsaved data will be lost.",
        ),
        _ => (
            format!("End “{name}”?"),
            "End Process",
            "The process is asked to quit. Unsaved data may be lost.",
        ),
    };
    let d = adw::AlertDialog::new(Some(&heading), Some(&format!("PID {pid}. {body}")));
    d.add_response("cancel", "Cancel");
    d.add_response("go", verb);
    d.set_response_appearance("go", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    let name = name.to_string();
    d.connect_response(None, move |_, resp| {
        if resp == "go" {
            do_signal(pid, start, name.clone(), sig);
        }
    });
    d.present(app::active_window().as_ref());
}

fn do_signal(pid: i32, start: u64, name: String, sig: Sig) {
    match actions::signal(pid, start, sig) {
        Ok(()) => app::sampler().scan_now(),
        Err(ActionError::PermissionDenied) => {
            let d = adw::AlertDialog::new(
                Some("Administrator Privileges Needed"),
                Some(&format!("“{name}” (PID {pid}) belongs to another user. Send {} as administrator?", sig.name())),
            );
            d.add_response("cancel", "Cancel");
            d.add_response("admin", "Authenticate…");
            d.set_response_appearance("admin", adw::ResponseAppearance::Suggested);
            d.set_close_response("cancel");
            d.connect_response(None, move |_, resp| {
                if resp == "admin" {
                    spawn_privileged(actions::privileged_signal_argv(pid, sig));
                }
            });
            d.present(app::active_window().as_ref());
        }
        Err(ActionError::Gone) => app::toast(&format!("“{name}” has already exited")),
        Err(e) => app::toast(&format!("Couldn’t signal “{name}”: {e}")),
    }
}

fn renice(pid: i32, start: u64, nice: i32) {
    match actions::set_nice(pid, start, nice) {
        Ok(()) => app::sampler().scan_now(),
        // Raising priority (lower nice) needs CAP_SYS_NICE.
        Err(ActionError::PermissionDenied) => spawn_privileged(actions::privileged_nice_argv(pid, nice)),
        Err(ActionError::Gone) => app::toast("The process has already exited"),
        Err(e) => app::toast(&format!("Couldn’t change priority: {e}")),
    }
}

fn spawn_privileged(argv: Vec<String>) {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    match gio::Subprocess::newv(&args, gio::SubprocessFlags::STDERR_SILENCE) {
        Ok(p) => p.wait_check_async(gio::Cancellable::NONE, |r| {
            match r {
                Ok(()) => app::sampler().scan_now(),
                // 126 = dialog dismissed, 127 = not authorized.
                Err(e) if !e.message().contains("126") => {
                    app::toast(&format!("Administrator action failed: {}", e.message()))
                }
                Err(_) => {}
            }
        }),
        Err(e) => app::toast(&format!("Couldn’t run pkexec: {}", e.message())),
    }
}
