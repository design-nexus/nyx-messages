//! Nyx Messages — SMS/RCS via Google Messages (nyx-messagesd).

mod bg;
mod composer;
mod icons;
mod link_preview;
mod model;
mod pair;
mod prefs;
mod service;
mod settings_dialog;
mod theme;
mod widgets;
mod window;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

pub const APP_ID: &str = "io.github.design_nexus.nyx.Messages";

/// The ID this copy runs under (`NYX_MESSAGES_APP_ID` gives a test copy its own).
pub fn app_id() -> String {
    std::env::var("NYX_MESSAGES_APP_ID").unwrap_or_else(|_| APP_ID.to_string())
}

const USAGE: &str = "Usage: nyx-messages [OPTIONS]\n\
\n\
  --toggle    close the window if it's open, otherwise open it\n\
  --help      show this help\n";

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-messages", name: "Messages" });

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }

    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }

    let app = gtk::Application::builder().application_id(app_id()).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();

    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        if argv.iter().any(|a| a == "--toggle")
            && let Some(w) = app.active_window()
        {
            w.close();
            return glib::ExitCode::SUCCESS;
        }
        window::present(app);
        glib::ExitCode::SUCCESS
    });

    app.connect_activate(|app| {
        window::present(app);
    });

    app.run()
}
