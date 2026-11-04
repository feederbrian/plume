//! A layout that never reflows because of content: one or two equal-width
//! columns, chosen from the available width alone. (GtkFlowBox decides from
//! children's natural widths, so a label changing from "9.8 %" to "10 %"
//! could resize or move every card and the graphs inside them.)
//!
//! Tiles are reordered by dragging a handle: any descendant with the
//! [`HANDLE`] style class, such as a card's header. Nothing moves while
//! dragging. The dragged tile dims, an outline marks the slot it will take,
//! and the order changes once, on release. Near the top or bottom edge of an
//! enclosing scrolled window, the view scrolls.

use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};
use std::cell::{Cell, OnceCell, RefCell};

/// Style class that marks a tile's drag handle.
pub const HANDLE: &str = "plume-drag-handle";

/// Auto-scroll starts this close to the scrolled window's top or bottom edge (px)…
const EDGE: f64 = 64.0;
/// …and reaches this speed at the edge or beyond it (px/s).
const MAX_SPEED: f64 = 1600.0;

type ReorderFn = Box<dyn Fn(Vec<String>)>;

struct Drag {
    /// Slots (indices among visible tiles): the dragged tile's, and the one it would take.
    from: usize,
    target: usize,
    /// The grabbed handle and its own cursor, restored afterwards.
    handle: gtk::Widget,
    handle_cursor: Option<gdk::Cursor>,
    /// Past the drag threshold: a real drag, not a click.
    active: bool,
    /// Pointer in the enclosing scrolled window's coordinates (stays put while the content scrolls).
    pointer: Option<(f64, f64)>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Tiles {
        /// Tiles in display order, with their keys.
        pub children: RefCell<Vec<(gtk::Widget, String)>>,
        pub min_col: Cell<i32>,
        pub spacing: Cell<i32>,
        /// Visible tiles from the last allocation: (index into `children`, rectangle).
        pub slots: RefCell<Vec<(usize, graphene::Rect)>>,
        /// Drop-target outline; always the last child, so it draws on top.
        pub outline: OnceCell<gtk::Widget>,
        pub(super) drag: RefCell<Option<Drag>>,
        pub scroll_tick: RefCell<Option<gtk::TickCallbackId>>,
        pub last_frame_us: Cell<i64>,
        pub(super) on_reorder: RefCell<Option<ReorderFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tiles {
        const NAME: &'static str = "PlumeTiles";
        type Type = super::Tiles;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Tiles {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let outline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            outline.add_css_class("plume-drop-outline");
            outline.set_can_target(false);
            outline.set_visible(false);
            outline.set_parent(&*obj);
            let _ = self.outline.set(outline.upcast());

            let g = gtk::GestureDrag::new();
            g.set_button(gdk::BUTTON_PRIMARY);
            g.connect_drag_begin(|g, x, y| {
                if let Some(t) = g.widget().and_downcast::<super::Tiles>() {
                    t.imp().drag_begin(g, x, y);
                }
            });
            g.connect_drag_update(|g, dx, dy| {
                if let Some(t) = g.widget().and_downcast::<super::Tiles>() {
                    t.imp().drag_update(g, dx, dy);
                }
            });
            g.connect_drag_end(|g, _, _| {
                if let Some(t) = g.widget().and_downcast::<super::Tiles>() {
                    t.imp().finish(true);
                }
            });
            // Also emitted when something else takes the pointer (e.g. touch scrolling).
            g.connect_cancel(|g, _| {
                if let Some(t) = g.widget().and_downcast::<super::Tiles>() {
                    t.imp().finish(false);
                }
            });
            obj.add_controller(g);
        }

        fn dispose(&self) {
            if let Some(id) = self.scroll_tick.take() {
                id.remove();
            }
            self.drag.take();
            for (c, _) in self.children.take() {
                c.unparent();
            }
            if let Some(o) = self.outline.get() {
                o.unparent();
            }
        }
    }

    impl Tiles {
        fn cols_for(&self, width: i32) -> i32 {
            let (m, s) = (self.min_col.get(), self.spacing.get());
            if width >= 2 * m + s { 2 } else { 1 }
        }

        fn visible(&self) -> Vec<usize> {
            let kids = self.children.borrow();
            (0..kids.len()).filter(|&i| kids[i].0.is_visible()).collect()
        }

        /// Row heights for a given width (natural height of the tallest tile in each row).
        fn rows(&self, width: i32) -> (i32, i32, Vec<i32>) {
            let cols = self.cols_for(width);
            let s = self.spacing.get();
            let col_w = ((width - s * (cols - 1)) / cols).max(1);
            let vis = self.visible();
            let kids = self.children.borrow();
            let rows = vis
                .chunks(cols as usize)
                .map(|row| row.iter().map(|&i| kids[i].0.measure(gtk::Orientation::Vertical, col_w).1).max().unwrap_or(0))
                .collect();
            (cols, col_w, rows)
        }

        /// The slot whose tile was pressed on a drag handle, and that handle.
        fn handle_at(&self, x: f64, y: f64) -> Option<(usize, gtk::Widget)> {
            let obj = self.obj();
            let me: &gtk::Widget = obj.upcast_ref();
            let mut w = obj.pick(x, y, gtk::PickFlags::DEFAULT)?;
            let mut handle = None;
            loop {
                if handle.is_none() && w.has_css_class(HANDLE) {
                    handle = Some(w.clone());
                }
                let parent = w.parent()?;
                if &parent == me {
                    let handle = handle?;
                    let idx = self.children.borrow().iter().position(|(c, _)| *c == w)?;
                    let slot = self.slots.borrow().iter().position(|(i, _)| *i == idx)?;
                    return Some((slot, handle));
                }
                w = parent;
            }
        }

        fn drag_begin(&self, g: &gtk::GestureDrag, x: f64, y: f64) {
            self.finish(false); // a drag that never ended (shouldn't happen)
            match self.handle_at(x, y) {
                Some((slot, handle)) => {
                    let handle_cursor = handle.cursor();
                    self.drag.replace(Some(Drag { from: slot, target: slot, handle, handle_cursor, active: false, pointer: None }));
                }
                None => {
                    g.set_state(gtk::EventSequenceState::Denied);
                }
            }
        }

        fn drag_update(&self, g: &gtk::GestureDrag, dx: f64, dy: f64) {
            let Some((x0, y0)) = g.start_point() else { return };
            let started = {
                let mut d = self.drag.borrow_mut();
                let Some(d) = d.as_mut() else { return };
                if d.active {
                    false
                } else {
                    let threshold = gtk::Settings::default().map_or(8, |s| s.gtk_dnd_drag_threshold()).max(1) as f64;
                    if dx.hypot(dy) < threshold {
                        return;
                    }
                    d.active = true;
                    true
                }
            };
            if started {
                g.set_state(gtk::EventSequenceState::Claimed);
                self.show_drag();
            }
            self.pointer_moved(x0 + dx, y0 + dy);
        }

        /// Dims the dragged tile and shows the outline. Nothing else changes until release.
        fn show_drag(&self) {
            let (from, handle) = match self.drag.borrow().as_ref() {
                Some(d) => (d.from, d.handle.clone()),
                None => return,
            };
            if let Some(&(i, _)) = self.slots.borrow().get(from) {
                self.children.borrow()[i].0.set_opacity(0.45);
            }
            handle.set_cursor_from_name(Some("grabbing"));
            if let Some(o) = self.outline.get() {
                o.set_visible(true);
            }
            self.obj().queue_allocate();
        }

        /// `x`, `y`: the pointer in this widget's coordinates.
        fn pointer_moved(&self, x: f64, y: f64) {
            let obj = self.obj();
            let in_scroller = obj
                .ancestor(gtk::ScrolledWindow::static_type())
                .and_then(|sw| obj.compute_point(&sw, &graphene::Point::new(x as f32, y as f32)))
                .map(|p| (p.x() as f64, p.y() as f64));
            if let Some(d) = self.drag.borrow_mut().as_mut() {
                d.pointer = in_scroller;
            }
            self.retarget(x, y);
            self.update_autoscroll();
        }

        /// Points the outline at the slot nearest to the pointer.
        fn retarget(&self, x: f64, y: f64) {
            let nearest = {
                let slots = self.slots.borrow();
                slots
                    .iter()
                    .enumerate()
                    .map(|(k, (_, r))| {
                        let (l, t) = (r.x() as f64, r.y() as f64);
                        let (rt, b) = (l + r.width() as f64, t + r.height() as f64);
                        let dx = (l - x).max(x - rt).max(0.0);
                        let dy = (t - y).max(y - b).max(0.0);
                        (k, dx * dx + dy * dy)
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(k, _)| k)
            };
            let changed = match (self.drag.borrow_mut().as_mut(), nearest) {
                (Some(d), Some(k)) if d.target != k => {
                    d.target = k;
                    true
                }
                _ => false,
            };
            if changed {
                self.obj().queue_allocate();
            }
        }

        /// Signed auto-scroll speed in px/s; 0 away from the edges.
        fn scroll_speed(&self) -> f64 {
            let Some(sw) = self.obj().ancestor(gtk::ScrolledWindow::static_type()) else { return 0.0 };
            let y = match self.drag.borrow().as_ref() {
                Some(Drag { active: true, pointer: Some((_, y)), .. }) => *y,
                _ => return 0.0,
            };
            let h = sw.height() as f64;
            let edge = EDGE.min(h / 4.0).max(1.0);
            let f = if y < edge {
                -(edge - y) / edge
            } else if y > h - edge {
                (y - (h - edge)) / edge
            } else {
                0.0
            };
            let f = f.clamp(-1.0, 1.0);
            f * f.abs() * MAX_SPEED
        }

        fn update_autoscroll(&self) {
            if self.scroll_speed() == 0.0 {
                if let Some(id) = self.scroll_tick.take() {
                    id.remove();
                }
                return;
            }
            if self.scroll_tick.borrow().is_some() {
                return;
            }
            self.last_frame_us.set(0);
            let id = self.obj().add_tick_callback(|t, clock| {
                let imp = t.imp();
                let v = imp.scroll_speed();
                if v == 0.0 {
                    imp.scroll_tick.take();
                    return glib::ControlFlow::Break;
                }
                let now = clock.frame_time();
                let last = imp.last_frame_us.replace(now);
                let dt = if last == 0 { 0.0 } else { ((now - last) as f64 / 1e6).min(0.05) };
                let Some(sw) = t.ancestor(gtk::ScrolledWindow::static_type()).and_downcast::<gtk::ScrolledWindow>() else {
                    return glib::ControlFlow::Continue;
                };
                let adj = sw.vadjustment();
                let before = adj.value();
                adj.set_value((before + v * dt).clamp(adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower())));
                if adj.value() != before {
                    // The content moved under a still pointer.
                    let ptr = imp.drag.borrow().as_ref().and_then(|d| d.pointer);
                    if let Some(p) = ptr.and_then(|(x, y)| sw.compute_point(t, &graphene::Point::new(x as f32, y as f32))) {
                        imp.retarget(p.x() as f64, p.y() as f64);
                    }
                }
                glib::ControlFlow::Continue
            });
            self.scroll_tick.replace(Some(id));
        }

        /// Ends a drag; with `commit`, moves the tile to the outlined slot.
        fn finish(&self, commit: bool) {
            let Some(d) = self.drag.take() else { return };
            if let Some(id) = self.scroll_tick.take() {
                id.remove();
            }
            if !d.active {
                return;
            }
            d.handle.set_cursor(d.handle_cursor.as_ref());
            if let Some(o) = self.outline.get() {
                o.set_visible(false);
            }
            let (from, to) = {
                let slots = self.slots.borrow();
                (slots.get(d.from).map(|s| s.0), slots.get(d.target).map(|s| s.0))
            };
            if let Some(f) = from {
                self.children.borrow()[f].0.set_opacity(1.0);
            }
            let (Some(f), Some(t)) = (from, to) else { return };
            if !commit || f == t {
                return;
            }
            {
                let mut kids = self.children.borrow_mut();
                let item = kids.remove(f);
                kids.insert(t, item);
            }
            self.restack();
            self.obj().queue_resize();
            let keys: Vec<String> = self.children.borrow().iter().map(|(_, k)| k.clone()).collect();
            if let Some(cb) = self.on_reorder.borrow().as_ref() {
                cb(keys);
            }
        }

        /// Keeps the widget tree (focus and accessibility order) in display order,
        /// with the outline last.
        pub(super) fn restack(&self) {
            let obj = self.obj();
            let outline = self.outline.get();
            for (c, _) in self.children.borrow().iter() {
                c.insert_before(&*obj, outline);
            }
        }
    }

    impl WidgetImpl for Tiles {
        fn unmap(&self) {
            // Page switched mid-drag: put everything back.
            self.finish(false);
            self.parent_unmap();
        }

        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (m, s) = (self.min_col.get(), self.spacing.get());
            match orientation {
                // Independent of content: one column minimum, two preferred.
                gtk::Orientation::Horizontal => (m, 2 * m + s, -1, -1),
                _ => {
                    let width = if for_size > 0 { for_size } else { 2 * m + s };
                    let (_, _, rows) = self.rows(width);
                    let h = rows.iter().sum::<i32>() + s * (rows.len() as i32 - 1).max(0);
                    (h, h, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let (cols, col_w, rows) = self.rows(width);
            let s = self.spacing.get();
            let vis = self.visible();
            let kids = self.children.borrow();
            let at = |x: f32, y: f32| Some(gsk::Transform::new().translate(&graphene::Point::new(x, y)));
            let mut slots = Vec::with_capacity(vis.len());
            let mut y = 0;
            for (r, row) in vis.chunks(cols as usize).enumerate() {
                let h = rows[r];
                for (c, &i) in row.iter().enumerate() {
                    let x = c as i32 * (col_w + s);
                    kids[i].0.allocate(col_w, h, -1, at(x as f32, y as f32));
                    slots.push((i, graphene::Rect::new(x as f32, y as f32, col_w as f32, h as f32)));
                }
                y += h + s;
            }
            if let Some(o) = self.outline.get().filter(|o| o.is_visible()) {
                let (min_w, ..) = o.measure(gtk::Orientation::Horizontal, -1);
                let (min_h, ..) = o.measure(gtk::Orientation::Vertical, -1);
                let target = self.drag.borrow().as_ref().map(|d| d.target.min(slots.len().saturating_sub(1)));
                match target.and_then(|t| slots.get(t)).map(|s| s.1) {
                    Some(r) => o.allocate((r.width() as i32).max(min_w), (r.height() as i32).max(min_h), -1, at(r.x(), r.y())),
                    None => o.allocate(min_w, min_h, -1, None),
                }
            }
            self.slots.replace(slots);
        }
    }
}

glib::wrapper! {
    pub struct Tiles(ObjectSubclass<imp::Tiles>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Tiles {
    pub fn new(min_col: i32, spacing: i32) -> Tiles {
        let t: Tiles = glib::Object::new();
        t.imp().min_col.set(min_col);
        t.imp().spacing.set(spacing);
        t
    }

    /// Adds a tile; `key` identifies it in [`Tiles::set_order`] and reorder callbacks.
    pub fn append(&self, child: &impl IsA<gtk::Widget>, key: &str) {
        child.insert_before(self, self.imp().outline.get());
        self.imp().children.borrow_mut().push((child.clone().upcast(), key.to_string()));
        self.queue_resize();
    }

    /// Puts tiles in `order` (by key). A tile it doesn't name (say, a GPU that
    /// wasn't there before) goes right after its default predecessor.
    pub fn set_order(&self, order: &[String]) {
        let imp = self.imp();
        let mut kids: Vec<Option<(gtk::Widget, String)>> = imp.children.take().into_iter().map(Some).collect();
        let keys: Vec<&str> = kids.iter().flatten().map(|(_, k)| k.as_str()).collect();
        let idx = merged_order(&keys, order);
        let out = idx.into_iter().filter_map(|i| kids[i].take()).collect();
        imp.children.replace(out);
        imp.restack();
        self.queue_resize();
    }

    /// Called with every tile's key, in the new order, after the user moves one.
    pub fn connect_reordered(&self, f: impl Fn(Vec<String>) + 'static) {
        self.imp().on_reorder.replace(Some(Box::new(f)));
    }
}

/// Display order for tiles with `default` keys, given a saved `order`: saved
/// keys first, in saved order (unknown ones ignored), then each unsaved key
/// right after its nearest default predecessor. Returns indices into `default`.
fn merged_order(default: &[&str], order: &[String]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::with_capacity(default.len());
    for k in order {
        if let Some(i) = default.iter().position(|d| d == k) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    for i in 0..default.len() {
        if out.contains(&i) {
            continue;
        }
        let after = (0..i).rev().find_map(|p| out.iter().position(|&o| o == p));
        out.insert(after.map_or(0, |a| a + 1), i);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::merged_order;

    fn keys(order: &[usize], default: &[&str]) -> Vec<String> {
        order.iter().map(|&i| default[i].to_string()).collect()
    }

    #[test]
    fn saved_order_wins_and_new_tiles_follow_their_neighbours() {
        let d = ["cpu", "mem", "gpu0", "gpu1", "net", "disk"];
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // No saved order: default.
        assert_eq!(keys(&merged_order(&d, &[]), &d), s(&d));
        // Full saved order.
        let full = s(&["net", "cpu", "disk", "gpu1", "mem", "gpu0"]);
        assert_eq!(keys(&merged_order(&d, &full), &d), full);
        // gpu1 is new (not saved): it goes right after gpu0, its default predecessor.
        let saved = s(&["disk", "gpu0", "cpu", "mem", "net"]);
        assert_eq!(keys(&merged_order(&d, &saved), &d), s(&["disk", "gpu0", "gpu1", "cpu", "mem", "net"]));
        // A new first tile goes first; unknown and duplicate saved keys are ignored.
        let saved = s(&["mem", "gone", "net", "mem", "disk", "gpu0", "gpu1"]);
        assert_eq!(keys(&merged_order(&d, &saved), &d), s(&["cpu", "mem", "net", "disk", "gpu0", "gpu1"]));
    }
}
