//! Plume — a featherweight GNOME system monitor.

mod app;
mod floats;
mod format;
mod inspector;
mod pages;
mod prefs;
mod profile;
mod settings;
mod theme;
mod widgets;

pub const APP_ID: &str = "io.github.feederbrian.Plume";

fn main() -> gtk::glib::ExitCode {
    // Answer --help/--version locally, without starting GTK or the sampler.
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--help" | "-h" => {
                print!("{}", app::HELP);
                return gtk::glib::ExitCode::SUCCESS;
            }
            "--version" => {
                println!("plume {}", env!("CARGO_PKG_VERSION"));
                return gtk::glib::ExitCode::SUCCESS;
            }
            _ => {}
        }
    }
    app::run()
}
