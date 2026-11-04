//! Card scaffolding for the Overview dashboard.
//!
//! Layout stability rules (values change four times a second): every value
//! label has a fixed width and tabular digits, and stats sit in a fixed
//! 4-column grid, so changing numbers never move or resize anything.

use gtk::prelude::*;
use std::cell::Cell;

pub struct Card {
    pub root: gtk::Box,
    pub sub: gtk::Label,
    pub value: gtk::Label,
    pub header_extra: gtk::Box,
    pub body: gtk::Box,
    pub stats: gtk::Grid,
    n_stats: Cell<i32>,
}

pub const STAT_COLUMNS: i32 = 4;

/// `value_chars`: fixed width of the header value, in characters.
pub fn card(title: &str, float_id: Option<&str>, value_chars: i32) -> Card {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("card");
    root.add_css_class("plume-card");
    root.set_hexpand(true);

    // The header is the card's drag handle (see widgets::tiles): an open hand
    // on hover, except over its buttons.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class(crate::widgets::tiles::HANDLE);
    header.set_cursor_from_name(Some("grab"));
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_hexpand(true);
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.add_css_class("plume-card-title");
    let sub = gtk::Label::new(None);
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_width_chars(1);
    sub.add_css_class("plume-card-sub");
    titles.append(&t);
    titles.append(&sub);
    header.append(&titles);

    let value = gtk::Label::new(None);
    value.add_css_class("plume-value");
    value.set_valign(gtk::Align::Center);
    value.set_xalign(1.0);
    fixed_width(&value, value_chars);
    header.append(&value);

    let header_extra = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header_extra.set_valign(gtk::Align::Center);
    header_extra.set_cursor_from_name(Some("default"));
    header.append(&header_extra);

    if let Some(id) = float_id {
        let b = gtk::Button::from_icon_name("window-new-symbolic");
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some("Pop out as a floating readout"));
        b.set_action_name(Some("app.float"));
        b.set_action_target_value(Some(&id.to_variant()));
        b.set_cursor_from_name(Some("default"));
        header.append(&b);
    }
    root.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.append(&body);

    let stats = gtk::Grid::new();
    stats.set_column_homogeneous(true);
    stats.set_column_spacing(12);
    stats.set_row_spacing(6);
    root.append(&stats);

    Card { root, sub, value, header_extra, body, stats, n_stats: Cell::new(0) }
}

/// Pins a label to exactly `chars` characters (ellipsizing anything longer).
pub fn fixed_width(label: &gtk::Label, chars: i32) {
    label.set_width_chars(chars);
    label.set_max_width_chars(chars);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
}

/// Adds a caption/value pair to the card's stats grid; returns the value label.
pub fn stat(card: &Card, caption: &str, swatch: Option<&str>) -> gtk::Label {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let cap_row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    if let Some(cls) = swatch {
        let sw = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        sw.add_css_class("plume-swatch");
        sw.add_css_class(cls);
        sw.set_valign(gtk::Align::Center);
        cap_row.append(&sw);
    }
    let cap = gtk::Label::new(Some(caption));
    cap.set_xalign(0.0);
    cap.add_css_class("plume-cap");
    cap.set_ellipsize(gtk::pango::EllipsizeMode::End);
    cap.set_width_chars(1);
    cap_row.append(&cap);
    let v = gtk::Label::new(Some("–"));
    v.set_xalign(0.0);
    v.add_css_class("plume-statval");
    // Natural width independent of the text: the grid columns are
    // homogeneous, so a long value must not widen the card.
    v.set_ellipsize(gtk::pango::EllipsizeMode::End);
    v.set_width_chars(1);
    b.append(&cap_row);
    b.append(&v);
    let n = card.n_stats.get();
    card.stats.attach(&b, n % STAT_COLUMNS, n / STAT_COLUMNS, 1, 1);
    card.n_stats.set(n + 1);
    v
}

/// Wraps a graph in an overlay with an optional corner label.
pub fn graph_frame(graph: &impl IsA<gtk::Widget>, height: i32) -> (gtk::Overlay, gtk::Label) {
    let o = gtk::Overlay::new();
    o.set_child(Some(graph));
    o.set_size_request(-1, height);
    let corner = gtk::Label::new(None);
    corner.add_css_class("plume-ymax");
    corner.set_halign(gtk::Align::Start);
    corner.set_valign(gtk::Align::Start);
    corner.set_can_target(false);
    o.add_overlay(&corner);
    (o, corner)
}

/// Sets text only if it differs (avoids needless relayout).
pub fn set(label: &gtk::Label, text: &str) {
    if label.text() != text {
        label.set_text(text);
    }
}

/// Like `set`, and also shows the full text as a tooltip (for long,
/// slowly-changing values such as device lists).
pub fn set_with_tip(label: &gtk::Label, text: &str) {
    if label.text() != text {
        label.set_text(text);
        label.set_tooltip_text(Some(text));
    }
}
