//! Cairn — the window.
//!
//! A single-window application: a list of installations, one action per row, and a
//! bottom bar that reports what the current run is doing. There are no preferences
//! to speak of (two settings), so everything lives in a menu rather than a sidebar.
//!
//! The command line tool `cairn` shares the whole engine underneath, which is what
//! makes this thing testable at all: a window cannot be checked in CI, but the code
//! behind every button can.

mod window;
mod worker;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(cairn_core::APP_ID)
        // Not DBusActivatable: a launch should return immediately, and GApplication's
        // single-instance handling still routes a second launch to the open window.
        .flags(gio::ApplicationFlags::empty())
        .build();

    app.connect_startup(|_| load_css());

    app.connect_activate(|app| {
        window::CairnWindow::build(app).present();
    });

    // `--version` has to work before a display exists, which matters for the
    // packaging check that proves the installed bundle is the version it claims.
    if std::env::args().any(|a| a == "--version" || a == "-v") {
        println!("{} {}", cairn_core::APP_NAME, cairn_core::VERSION);
        return glib::ExitCode::SUCCESS;
    }

    app.run()
}

/// A few rules that libadwaita has no class for.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        "
        .cairn-path {
            font-feature-settings: 'tnum';
        }
        progressbar.cairn-thin trough {
            min-height: 4px;
        }
        ",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
