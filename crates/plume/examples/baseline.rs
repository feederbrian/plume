//! Footprint baseline: an empty libadwaita window with a header bar.
//! Compare `plume` against this to separate toolkit cost from app cost.
use adw::prelude::*;

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id("io.github.feederbrian.Baseline").build();
    app.connect_activate(|app| {
        let w = adw::ApplicationWindow::new(app);
        w.set_default_size(1300, 900);
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&gtk::Label::new(Some("baseline"))));
        w.set_content(Some(&tv));
        w.present();
    });
    app.run()
}
