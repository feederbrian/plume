//! Themes: libadwaita colour overrides + graph palettes.
//!
//! "System" follows GNOME's light/dark preference with Adwaita colours. The
//! others force a scheme and override libadwaita's named colours (both the
//! `@define-color` names and, on GTK ≥ 4.16 / libadwaita ≥ 1.6, the CSS
//! variables) so every widget picks them up.

use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    /// None = keep libadwaita's own colours (System theme).
    pub ui: Option<Ui>,
    pub fg: u32,
    pub cpu: u32,
    pub kernel: u32,
    pub mem: u32,
    pub swap: u32,
    pub gpu: u32,
    pub rx: u32,
    pub tx: u32,
    pub read: u32,
    pub write: u32,
    pub cores: [u32; 8],
}

#[derive(Clone, Copy, Debug)]
pub struct Ui {
    pub window: u32,
    pub view: u32,
    pub card: u32,
    pub header: u32,
    pub popover: u32,
    pub fg: u32,
    pub accent: u32,
    pub accent_fg: u32,
    pub destructive: u32,
}

const fn ui(window: u32, view: u32, card: u32, header: u32, popover: u32, fg: u32, accent: u32, accent_fg: u32, destructive: u32) -> Option<Ui> {
    Some(Ui { window, view, card, header, popover, fg, accent, accent_fg, destructive })
}

pub const THEMES: &[Palette] = &[
    Palette {
        id: "system", name: "System (Adwaita)", dark: false, ui: None, fg: 0x1e1e1e,
        cpu: 0x3584e4, kernel: 0xe01b24, mem: 0x9141ac, swap: 0xe66100, gpu: 0x2ec27e,
        rx: 0x1c71d8, tx: 0xe66100, read: 0x26a269, write: 0xc01c28,
        cores: [0x3584e4, 0x2ec27e, 0xe5a50a, 0xe66100, 0xc01c28, 0x9141ac, 0x865e3c, 0x1c71d8],
    },
    Palette {
        id: "adwaita-dark", name: "Adwaita Dark", dark: true, ui: None, fg: 0xffffff,
        cpu: 0x62a0ea, kernel: 0xf66151, mem: 0xc061cb, swap: 0xffa348, gpu: 0x57e389,
        rx: 0x62a0ea, tx: 0xffa348, read: 0x57e389, write: 0xf66151,
        cores: [0x62a0ea, 0x57e389, 0xf8e45c, 0xffa348, 0xf66151, 0xc061cb, 0xcdab8f, 0x99c1f1],
    },
    Palette {
        id: "adwaita-light", name: "Adwaita Light", dark: false, ui: None, fg: 0x1e1e1e,
        cpu: 0x3584e4, kernel: 0xe01b24, mem: 0x9141ac, swap: 0xe66100, gpu: 0x2ec27e,
        rx: 0x1c71d8, tx: 0xe66100, read: 0x26a269, write: 0xc01c28,
        cores: [0x3584e4, 0x2ec27e, 0xe5a50a, 0xe66100, 0xc01c28, 0x9141ac, 0x865e3c, 0x1c71d8],
    },
    Palette {
        id: "tokyo-night", name: "Tokyo Night", dark: true,
        ui: ui(0x1a1b26, 0x16161e, 0x24283b, 0x16161e, 0x1f2335, 0xc0caf5, 0x7aa2f7, 0x1a1b26, 0xf7768e),
        fg: 0xc0caf5, cpu: 0x7aa2f7, kernel: 0xf7768e, mem: 0xbb9af7, swap: 0xff9e64, gpu: 0x9ece6a,
        rx: 0x7dcfff, tx: 0xff9e64, read: 0x9ece6a, write: 0xf7768e,
        cores: [0x7aa2f7, 0x9ece6a, 0xe0af68, 0xff9e64, 0xf7768e, 0xbb9af7, 0x7dcfff, 0x73daca],
    },
    Palette {
        id: "tokyo-night-storm", name: "Tokyo Night Storm", dark: true,
        ui: ui(0x24283b, 0x1f2335, 0x292e42, 0x1f2335, 0x292e42, 0xc0caf5, 0x7aa2f7, 0x1f2335, 0xf7768e),
        fg: 0xc0caf5, cpu: 0x7aa2f7, kernel: 0xf7768e, mem: 0xbb9af7, swap: 0xff9e64, gpu: 0x9ece6a,
        rx: 0x7dcfff, tx: 0xff9e64, read: 0x9ece6a, write: 0xf7768e,
        cores: [0x7aa2f7, 0x9ece6a, 0xe0af68, 0xff9e64, 0xf7768e, 0xbb9af7, 0x7dcfff, 0x73daca],
    },
    Palette {
        id: "tokyo-night-day", name: "Tokyo Night Day", dark: false,
        ui: ui(0xe1e2e7, 0xe9e9ed, 0xd0d5e3, 0xd0d5e3, 0xe9e9ed, 0x3760bf, 0x2e7de9, 0xffffff, 0xf52a65),
        fg: 0x3760bf, cpu: 0x2e7de9, kernel: 0xf52a65, mem: 0x9854f1, swap: 0xb15c00, gpu: 0x587539,
        rx: 0x007197, tx: 0xb15c00, read: 0x587539, write: 0xf52a65,
        cores: [0x2e7de9, 0x587539, 0x8c6c3e, 0xb15c00, 0xf52a65, 0x9854f1, 0x007197, 0x118c74],
    },
    Palette {
        id: "solarized-dark", name: "Solarized Dark", dark: true,
        ui: ui(0x002b36, 0x00212b, 0x073642, 0x00212b, 0x073642, 0x93a1a1, 0x268bd2, 0xfdf6e3, 0xdc322f),
        fg: 0x93a1a1, cpu: 0x268bd2, kernel: 0xdc322f, mem: 0x6c71c4, swap: 0xcb4b16, gpu: 0x859900,
        rx: 0x2aa198, tx: 0xcb4b16, read: 0x859900, write: 0xdc322f,
        cores: [0x268bd2, 0x859900, 0xb58900, 0xcb4b16, 0xdc322f, 0xd33682, 0x6c71c4, 0x2aa198],
    },
    Palette {
        id: "solarized-light", name: "Solarized Light", dark: false,
        ui: ui(0xfdf6e3, 0xfffbf0, 0xeee8d5, 0xeee8d5, 0xfffbf0, 0x586e75, 0x268bd2, 0xfdf6e3, 0xdc322f),
        fg: 0x586e75, cpu: 0x268bd2, kernel: 0xdc322f, mem: 0x6c71c4, swap: 0xcb4b16, gpu: 0x859900,
        rx: 0x2aa198, tx: 0xcb4b16, read: 0x859900, write: 0xdc322f,
        cores: [0x268bd2, 0x859900, 0xb58900, 0xcb4b16, 0xdc322f, 0xd33682, 0x6c71c4, 0x2aa198],
    },
    Palette {
        id: "nord", name: "Nord", dark: true,
        ui: ui(0x2e3440, 0x292e39, 0x3b4252, 0x292e39, 0x3b4252, 0xeceff4, 0x88c0d0, 0x2e3440, 0xbf616a),
        fg: 0xeceff4, cpu: 0x88c0d0, kernel: 0xbf616a, mem: 0xb48ead, swap: 0xd08770, gpu: 0xa3be8c,
        rx: 0x81a1c1, tx: 0xd08770, read: 0xa3be8c, write: 0xbf616a,
        cores: [0x88c0d0, 0xa3be8c, 0xebcb8b, 0xd08770, 0xbf616a, 0xb48ead, 0x81a1c1, 0x8fbcbb],
    },
    Palette {
        id: "gruvbox-dark", name: "Gruvbox Dark", dark: true,
        ui: ui(0x282828, 0x1d2021, 0x3c3836, 0x1d2021, 0x3c3836, 0xebdbb2, 0xfabd2f, 0x282828, 0xfb4934),
        fg: 0xebdbb2, cpu: 0x83a598, kernel: 0xfb4934, mem: 0xd3869b, swap: 0xfe8019, gpu: 0xb8bb26,
        rx: 0x8ec07c, tx: 0xfe8019, read: 0xb8bb26, write: 0xfb4934,
        cores: [0x83a598, 0xb8bb26, 0xfabd2f, 0xfe8019, 0xfb4934, 0xd3869b, 0x8ec07c, 0xa89984],
    },
    Palette {
        id: "gruvbox-light", name: "Gruvbox Light", dark: false,
        ui: ui(0xfbf1c7, 0xf9f5d7, 0xebdbb2, 0xebdbb2, 0xf9f5d7, 0x3c3836, 0xaf3a03, 0xfbf1c7, 0x9d0006),
        fg: 0x3c3836, cpu: 0x076678, kernel: 0x9d0006, mem: 0x8f3f71, swap: 0xaf3a03, gpu: 0x79740e,
        rx: 0x427b58, tx: 0xaf3a03, read: 0x79740e, write: 0x9d0006,
        cores: [0x076678, 0x79740e, 0xb57614, 0xaf3a03, 0x9d0006, 0x8f3f71, 0x427b58, 0x7c6f64],
    },
    Palette {
        id: "dracula", name: "Dracula", dark: true,
        ui: ui(0x282a36, 0x21222c, 0x343746, 0x21222c, 0x343746, 0xf8f8f2, 0xbd93f9, 0x282a36, 0xff5555),
        fg: 0xf8f8f2, cpu: 0xbd93f9, kernel: 0xff5555, mem: 0xff79c6, swap: 0xffb86c, gpu: 0x50fa7b,
        rx: 0x8be9fd, tx: 0xffb86c, read: 0x50fa7b, write: 0xff5555,
        cores: [0xbd93f9, 0x50fa7b, 0xf1fa8c, 0xffb86c, 0xff5555, 0xff79c6, 0x8be9fd, 0x6272a4],
    },
    Palette {
        id: "catppuccin-mocha", name: "Catppuccin Mocha", dark: true,
        ui: ui(0x1e1e2e, 0x181825, 0x313244, 0x181825, 0x313244, 0xcdd6f4, 0xcba6f7, 0x1e1e2e, 0xf38ba8),
        fg: 0xcdd6f4, cpu: 0x89b4fa, kernel: 0xf38ba8, mem: 0xcba6f7, swap: 0xfab387, gpu: 0xa6e3a1,
        rx: 0x89dceb, tx: 0xfab387, read: 0xa6e3a1, write: 0xf38ba8,
        cores: [0x89b4fa, 0xa6e3a1, 0xf9e2af, 0xfab387, 0xf38ba8, 0xcba6f7, 0x89dceb, 0x94e2d5],
    },
    Palette {
        id: "catppuccin-latte", name: "Catppuccin Latte", dark: false,
        ui: ui(0xeff1f5, 0xffffff, 0xe6e9ef, 0xe6e9ef, 0xffffff, 0x4c4f69, 0x8839ef, 0xeff1f5, 0xd20f39),
        fg: 0x4c4f69, cpu: 0x1e66f5, kernel: 0xd20f39, mem: 0x8839ef, swap: 0xfe640b, gpu: 0x40a02b,
        rx: 0x209fb5, tx: 0xfe640b, read: 0x40a02b, write: 0xd20f39,
        cores: [0x1e66f5, 0x40a02b, 0xdf8e1d, 0xfe640b, 0xd20f39, 0x8839ef, 0x209fb5, 0x179299],
    },
    Palette {
        id: "one-dark", name: "One Dark", dark: true,
        ui: ui(0x282c34, 0x21252b, 0x2c313a, 0x21252b, 0x2c313a, 0xabb2bf, 0x61afef, 0x282c34, 0xe06c75),
        fg: 0xabb2bf, cpu: 0x61afef, kernel: 0xe06c75, mem: 0xc678dd, swap: 0xd19a66, gpu: 0x98c379,
        rx: 0x56b6c2, tx: 0xd19a66, read: 0x98c379, write: 0xe06c75,
        cores: [0x61afef, 0x98c379, 0xe5c07b, 0xd19a66, 0xe06c75, 0xc678dd, 0x56b6c2, 0xabb2bf],
    },
];

thread_local! {
    static ACTIVE: Cell<Palette> = const { Cell::new(THEMES[0]) };
    static GEN: Cell<u64> = const { Cell::new(1) };
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    static BASE: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

pub fn palette() -> Palette {
    ACTIVE.with(Cell::get)
}

/// Changes whenever colours change; graphs compare it to know when to redraw.
pub fn generation() -> u64 {
    GEN.with(Cell::get)
}

pub fn find(id: &str) -> Palette {
    *THEMES.iter().find(|t| t.id == id).unwrap_or(&THEMES[0])
}

pub fn rgba(c: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        ((c >> 16) & 0xff) as f32 / 255.0,
        ((c >> 8) & 0xff) as f32 / 255.0,
        (c & 0xff) as f32 / 255.0,
        alpha,
    )
}

fn hex(c: u32) -> String {
    format!("#{c:06x}")
}

/// Applies theme `id` app-wide. Call once at startup and whenever it changes.
pub fn apply(id: &str) {
    let sm = adw::StyleManager::default();
    let mut p = find(id);
    sm.set_color_scheme(match p.id {
        "system" => adw::ColorScheme::Default,
        _ if p.dark => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::ForceLight,
    });
    if p.id == "system" && sm.is_dark() {
        // Same palette as Adwaita Dark, but keep the "system" identity.
        let d = find("adwaita-dark");
        p = Palette { id: "system", name: THEMES[0].name, ..d };
    }
    ACTIVE.with(|a| a.set(p));
    GEN.with(|g| g.set(g.get() + 1));

    let css = p.ui.map(ui_css).unwrap_or_default();
    PROVIDER.with(|cell| {
        let mut cell = cell.borrow_mut();
        let provider = cell.get_or_insert_with(|| {
            let pr = gtk::CssProvider::new();
            if let Some(d) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(&d, &pr, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
            }
            pr
        });
        provider.load_from_string(&css);
    });
    load_base_css();
}

fn ui_css(u: Ui) -> String {
    let pairs = [
        ("window_bg_color", u.window),
        ("window_fg_color", u.fg),
        ("view_bg_color", u.view),
        ("view_fg_color", u.fg),
        ("headerbar_bg_color", u.header),
        ("headerbar_fg_color", u.fg),
        ("headerbar_backdrop_color", u.window),
        ("card_bg_color", u.card),
        ("card_fg_color", u.fg),
        ("popover_bg_color", u.popover),
        ("popover_fg_color", u.fg),
        ("dialog_bg_color", u.popover),
        ("dialog_fg_color", u.fg),
        ("sidebar_bg_color", u.header),
        ("sidebar_fg_color", u.fg),
        ("accent_bg_color", u.accent),
        ("accent_fg_color", u.accent_fg),
        ("accent_color", u.accent),
        ("destructive_bg_color", u.destructive),
        ("destructive_color", u.destructive),
    ];
    let mut css = String::new();
    for (k, v) in pairs {
        css.push_str(&format!("@define-color {k} {};\n", hex(v)));
    }
    // libadwaita ≥ 1.6 reads CSS variables instead of named colours.
    if gtk::minor_version() >= 16 {
        css.push_str(":root {\n");
        for (k, v) in pairs {
            css.push_str(&format!("  --{}: {};\n", k.replace('_', "-"), hex(v)));
        }
        css.push_str("}\n");
    }
    css
}

const BASE_CSS: &str = r#"
.plume-card { padding: 14px 16px 12px 16px; }
.plume-card-title { font-weight: 700; font-size: 1.08em; }
.plume-card-sub { opacity: 0.6; font-size: 0.9em; }
.plume-value { font-weight: 700; font-size: 1.45em; font-feature-settings: "tnum"; }
.plume-num { font-feature-settings: "tnum"; }
.plume-cap { opacity: 0.6; font-size: 0.82em; }
.plume-statval { font-feature-settings: "tnum"; font-weight: 600; }
.plume-graph { border-radius: 8px; }
.plume-core-label { font-size: 0.78em; opacity: 0.85; font-feature-settings: "tnum"; margin: 3px 6px; }
.plume-ymax { font-size: 0.75em; opacity: 0.55; margin: 3px 6px; font-feature-settings: "tnum"; }
.plume-graph-title { font-weight: 700; letter-spacing: 0.06em; font-size: 0.72em; }
.plume-summary .plume-statval { font-weight: 600; }
.plume-status { opacity: 0.7; font-size: 0.9em; font-feature-settings: "tnum"; }
.plume-asleep { opacity: 0.6; font-style: italic; }
.plume-swatch { min-width: 10px; min-height: 10px; border-radius: 3px; }
/* Cells give their padding to the label so a striped label spans the whole row. */
columnview.plume-procs > listview > row > cell { padding: 0; }
columnview.plume-procs label.plume-cell { padding: 4px 8px; }
columnview.plume-procs > listview > row:not(:selected):not(:hover) > cell > label.plume-odd { background-color: alpha(@view_fg_color, 0.055); }
window.plume-float { border-radius: 14px; }
window.plume-float .plume-float-title { font-weight: 700; font-size: 0.85em; opacity: 0.75; margin: 8px 4px 4px 12px; }
window.plume-float .plume-float-value { font-weight: 700; font-size: 1.1em; font-feature-settings: "tnum"; margin: 6px 12px 2px 4px; }
"#;

fn load_base_css() {
    let opacity = crate::settings::with(|s| s.float_opacity);
    let p = palette();
    let mut css = String::from(BASE_CSS);
    css.push_str(&format!(
        "window.plume-float {{ background-color: alpha(@window_bg_color, {opacity:.2}); }}\n\
         .plume-sw-cpu {{ background: {}; }} .plume-sw-kernel {{ background: {}; }} .plume-sw-mem {{ background: {}; }}\n\
         .plume-sw-swap {{ background: {}; }} .plume-sw-gpu {{ background: {}; }} .plume-sw-rx {{ background: {}; }}\n\
         .plume-sw-tx {{ background: {}; }} .plume-sw-read {{ background: {}; }} .plume-sw-write {{ background: {}; }}\n",
        hex(p.cpu), hex(p.kernel), hex(p.mem), hex(p.swap), hex(p.gpu), hex(p.rx), hex(p.tx), hex(p.read), hex(p.write)
    ));
    BASE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let provider = cell.get_or_insert_with(|| {
            let pr = gtk::CssProvider::new();
            if let Some(d) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(&d, &pr, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
            pr
        });
        provider.load_from_string(&css);
    });
}

/// Re-applies when the desktop switches light/dark (System theme only).
pub fn follow_system() {
    let sm = adw::StyleManager::default();
    sm.connect_dark_notify(|_| {
        if crate::settings::with(|s| s.theme == "system") {
            apply("system");
        }
    });
    let _ = glib::MainContext::default();
}

/// Refreshes the float opacity rule without touching colours.
pub fn reload_base() {
    load_base_css();
}
