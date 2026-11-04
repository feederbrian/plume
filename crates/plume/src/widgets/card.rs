//! Card scaffolding for the Resources dashboard.

use gtk::prelude::*;

pub struct Card {
    pub root: gtk::Box,
    pub sub: gtk::Label,
    pub value: gtk::Label,
    pub header_extra: gtk::Box,
    pub body: gtk::Box,
    pub stats: gtk::FlowBox,
}

pub fn card(title: &str, float_id: Option<&str>) -> Card {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("card");
    root.add_css_class("plume-card");
    root.set_hexpand(true);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_hexpand(true);
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.add_css_class("plume-card-title");
    let sub = gtk::Label::new(None);
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.add_css_class("plume-card-sub");
    titles.append(&t);
    titles.append(&sub);
    header.append(&titles);

    let value = gtk::Label::new(None);
    value.add_css_class("plume-value");
    value.set_valign(gtk::Align::Center);
    header.append(&value);

    let header_extra = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header_extra.set_valign(gtk::Align::Center);
    header.append(&header_extra);

    if let Some(id) = float_id {
        let b = gtk::Button::from_icon_name("window-new-symbolic");
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some("Pop out as a floating readout"));
        b.set_action_name(Some("app.float"));
        b.set_action_target_value(Some(&id.to_variant()));
        header.append(&b);
    }
    root.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.append(&body);

    let stats = gtk::FlowBox::new();
    stats.set_selection_mode(gtk::SelectionMode::None);
    stats.set_homogeneous(true);
    stats.set_min_children_per_line(2);
    stats.set_max_children_per_line(4);
    stats.set_column_spacing(12);
    stats.set_row_spacing(6);
    root.append(&stats);

    Card { root, sub, value, header_extra, body, stats }
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
    cap_row.append(&cap);
    let v = gtk::Label::new(Some("–"));
    v.set_xalign(0.0);
    v.set_ellipsize(gtk::pango::EllipsizeMode::End);
    // Cap natural width: the stats grid is homogeneous, so one long value
    // would otherwise widen every column and the whole card.
    v.set_max_width_chars(16);
    v.set_width_chars(8);
    cap.set_max_width_chars(16);
    cap.set_ellipsize(gtk::pango::EllipsizeMode::End);
    v.add_css_class("plume-statval");
    b.append(&cap_row);
    b.append(&v);
    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&b));
    child.set_focusable(false);
    card.stats.append(&child);
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
        if label.ellipsize() != gtk::pango::EllipsizeMode::None {
            label.set_tooltip_text(Some(text));
        }
    }
}
