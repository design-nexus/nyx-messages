//! Settings card: phone pairing + standard Appearance / Keyboard / About.

use crate::{prefs, service, widgets, window};
use gtk::prelude::*;
use nyx_ui::settings_card::{self, About, Spec, Standard};

pub use nyx_ui::settings_card::{escape, is_open};

const GROUPS: &[(&str, &str, &str)] = &[("Phone", "phone-symbolic", "Google Messages pairing")];

const SPEC: Spec = Spec {
    groups: GROUPS,
    search_text: None,
    standard: Some(Standard {
        app: "messages",
        about: About {
            name: "Messages",
            icon: "io.github.design_nexus.nyx.Messages",
            version: env!("CARGO_PKG_VERSION"),
            description: "SMS and RCS through Google Messages on your phone.",
            licence: "MIT",
            website: "https://github.com/design-nexus/nyx-messages",
            issues: "https://github.com/design-nexus/nyx-messages/issues",
            credits: &[("mautrix libgm", "Google Messages protocol (daemon)")],
        },
        shortcuts: window::SHORTCUTS,
        reduce_motion: || prefs::get().reduce_motion,
        set_reduce_motion: |on| prefs::update(|p| p.reduce_motion = on),
    }),
};

pub fn open() {
    let (Some(overlay), Some(win)) = (window::overlay(), window::window()) else {
        return;
    };
    settings_card::open(&overlay, &win, SPEC, || {
        let page = widgets::page("settings");
        build_phone(&page);
        (page.root.upcast(), page.body)
    });
}

fn build_phone(page: &widgets::Page) {
    let g = page.group("Phone");
    g.note("Chats stay on your phone. This app talks to Google Messages over a paired session.");
    let (row, _) = widgets::button_row(
        "Unpair phone",
        "Forget this computer in Google Messages and clear the local session.",
        "Unpair",
        |_| {
            crate::bg::run(
                || service::logout().map_err(|e| e.to_string()),
                |r: Result<(), String>| {
                    settings_card::close();
                    match r {
                        Ok(()) => {
                            crate::model::set_paired(false);
                            crate::model::replace_chats(&[]);
                            window::show_pair_after_unpair();
                        }
                        Err(e) => eprintln!("nyx-messages: unpair: {e}"),
                    }
                },
            );
        },
    );
    g.add(&row);
}
