//! Google Account pairing UI (WebKit cookie capture + emoji confirm).
//! WebKit is created only when the user clicks Sign in — not at app start.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use webkit6::prelude::*;

const GAIA_URL: &str =
    "https://accounts.google.com/AccountChooser?continue=https%3A%2F%2Fmessages.google.com%2Fweb%2Fauthentication";

const NEEDED: &[&str] = &["SID", "HSID", "SSID", "OSID", "APISID", "SAPISID"];

const COOKIE_URIS: &[&str] = &["https://messages.google.com/", "https://accounts.google.com/", "https://google.com/"];

pub struct PairPage {
    pub root: gtk::Box,
    stack: gtk::Stack,
    emoji_label: gtk::Label,
    status: gtk::Label,
    web_hint: gtk::Label,
    unpair: gtk::Button,
    web_box: gtk::Box,
    web: RefCell<Option<webkit6::WebView>>,
    session: RefCell<Option<webkit6::NetworkSession>>,
    pairing: RefCell<bool>,
    checking: RefCell<bool>,
    on_paired: Rc<dyn Fn()>,
}

impl PairPage {
    pub fn new(on_paired: impl Fn() + 'static) -> Rc<Self> {
        let on_paired = Rc::new(on_paired) as Rc<dyn Fn()>;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("messages-pair");
        root.set_hexpand(true);
        root.set_vexpand(true);

        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);

        let intro = gtk::Box::new(gtk::Orientation::Vertical, 14);
        intro.add_css_class("messages-pair-panel");
        intro.set_halign(gtk::Align::Center);
        intro.set_valign(gtk::Align::Center);
        intro.set_hexpand(true);
        intro.set_vexpand(true);
        intro.append(&gtk::Label::builder().label("Pair with Google Messages").css_classes(["messages-pair-title"]).build());
        intro.append(
            &gtk::Label::builder()
                .label("Sign in with the Google Account on your phone, then tap the matching emoji in Messages → Device pairing.")
                .wrap(true)
                .justify(gtk::Justification::Center)
                .max_width_chars(42)
                .css_classes(["messages-pair-body"])
                .build(),
        );
        let start = gtk::Button::with_label("Sign in with Google");
        start.add_css_class("suggested-action");
        start.set_halign(gtk::Align::Center);
        intro.append(&start);
        let unpair = gtk::Button::with_label("Unpair phone");
        unpair.add_css_class("flat");
        unpair.set_halign(gtk::Align::Center);
        unpair.set_visible(false);
        intro.append(&unpair);
        let status =
            gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).css_classes(["messages-pair-status"]).build();
        intro.append(&status);
        stack.add_named(&intro, Some("intro"));

        let web_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        web_box.set_hexpand(true);
        web_box.set_vexpand(true);
        let web_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        web_bar.add_css_class("messages-pair-webbar");
        let back = gtk::Button::with_label("Cancel");
        web_bar.append(&back);
        let web_hint = gtk::Label::builder()
            .label("Sign in with your Google Account — stay on this page until it finishes.")
            .hexpand(true)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["messages-pair-body"])
            .build();
        web_bar.append(&web_hint);
        web_box.append(&web_bar);
        // WebView is appended lazily on first Sign in.
        stack.add_named(&web_box, Some("web"));

        let emoji_panel = gtk::Box::new(gtk::Orientation::Vertical, 14);
        emoji_panel.add_css_class("messages-pair-panel");
        emoji_panel.set_halign(gtk::Align::Center);
        emoji_panel.set_valign(gtk::Align::Center);
        emoji_panel.set_hexpand(true);
        emoji_panel.set_vexpand(true);
        emoji_panel.append(&gtk::Label::builder().label("Confirm on your phone").css_classes(["messages-pair-title"]).build());
        emoji_panel.append(
            &gtk::Label::builder()
                .label("In Google Messages, open Device pairing and tap this emoji:")
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["messages-pair-body"])
                .build(),
        );
        let emoji_label = gtk::Label::builder().label("…").css_classes(["messages-pair-emoji"]).build();
        emoji_panel.append(&emoji_label);
        let emoji_cancel = gtk::Button::with_label("Cancel");
        emoji_cancel.add_css_class("flat");
        emoji_cancel.set_halign(gtk::Align::Center);
        emoji_panel.append(&emoji_cancel);
        stack.add_named(&emoji_panel, Some("emoji"));

        root.append(&stack);

        let page = Rc::new(Self {
            root,
            stack: stack.clone(),
            emoji_label,
            status: status.clone(),
            web_hint: web_hint.clone(),
            unpair: unpair.clone(),
            web_box: web_box.clone(),
            web: RefCell::new(None),
            session: RefCell::new(None),
            pairing: RefCell::new(false),
            checking: RefCell::new(false),
            on_paired: on_paired.clone(),
        });

        start.connect_clicked({
            let page = page.clone();
            move |_| {
                page.status.set_text("");
                page.pairing.replace(false);
                page.checking.replace(false);
                page.web_hint.set_text("Sign in with your Google Account — stay on this page until it finishes.");
                page.ensure_web();
                page.stack.set_visible_child_name("web");
                if let Some(web) = page.web.borrow().as_ref() {
                    web.load_uri(GAIA_URL);
                }
                page.start_cookie_poll();
            }
        });
        back.connect_clicked({
            let page = page.clone();
            move |_| {
                page.pairing.replace(false);
                page.checking.replace(false);
                crate::bg::run(
                    || {
                        if let Ok(mut c) = nyx_messages_proto::Client::connect_or_start() {
                            let _ = c.pair_cancel();
                        }
                    },
                    |_| {},
                );
                page.stack.set_visible_child_name("intro");
                if let Some(web) = page.web.borrow().as_ref() {
                    web.load_uri("about:blank");
                }
            }
        });
        emoji_cancel.connect_clicked({
            let page = page.clone();
            move |_| {
                page.pairing.replace(false);
                crate::bg::run(
                    || {
                        if let Ok(mut c) = nyx_messages_proto::Client::connect_or_start() {
                            let _ = c.pair_cancel();
                        }
                    },
                    |_| {},
                );
                page.show_intro_error("");
            }
        });
        unpair.connect_clicked({
            let page = page.clone();
            move |_| {
                page.status.set_text("Unpairing…");
                crate::bg::run(|| -> Result<(), String> { crate::service::logout().map_err(|e| e.to_string()) }, {
                    let page = page.clone();
                    move |r: Result<(), String>| match r {
                        Ok(()) => {
                            page.unpair.set_visible(false);
                            page.status.set_text("Phone unpaired.");
                        }
                        Err(e) => page.status.set_text(&e),
                    }
                });
            }
        });

        page
    }

    fn ensure_web(self: &Rc<Self>) {
        if self.web.borrow().is_some() {
            return;
        }
        let session = webkit6::NetworkSession::new_ephemeral();
        let web = webkit6::WebView::builder().network_session(&session).hexpand(true).vexpand(true).build();
        self.web_box.append(&web);
        web.connect_load_changed({
            let page = self.clone();
            move |_wv, ev| {
                if ev != webkit6::LoadEvent::Finished {
                    return;
                }
                page.try_capture_cookies();
            }
        });
        *self.session.borrow_mut() = Some(session);
        *self.web.borrow_mut() = Some(web);
    }

    pub fn show_emoji(&self, emoji: &str) {
        self.emoji_label.set_text(emoji);
        self.stack.set_visible_child_name("emoji");
    }

    pub fn show_intro_error(&self, msg: &str) {
        self.status.set_text(msg);
        self.stack.set_visible_child_name("intro");
        self.pairing.replace(false);
        self.checking.replace(false);
    }

    pub fn set_unpair_visible(&self, visible: bool) {
        self.unpair.set_visible(visible);
    }

    fn start_cookie_poll(self: &Rc<Self>) {
        let page = self.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            if page.stack.visible_child_name().as_deref() != Some("web") {
                return glib::ControlFlow::Break;
            }
            if *page.pairing.borrow() {
                return glib::ControlFlow::Break;
            }
            page.try_capture_cookies();
            glib::ControlFlow::Continue
        });
    }

    fn try_capture_cookies(self: &Rc<Self>) {
        if *self.pairing.borrow() || *self.checking.borrow() {
            return;
        }
        let Some(session) = self.session.borrow().clone() else {
            return;
        };
        let Some(mgr) = session.cookie_manager() else {
            return;
        };
        self.checking.replace(true);
        let page = self.clone();
        collect_cookies(mgr, COOKIE_URIS.to_vec(), HashMap::new(), 0, move |map| {
            page.checking.replace(false);
            if *page.pairing.borrow() {
                return;
            }
            if page.stack.visible_child_name().as_deref() != Some("web") {
                return;
            }
            for k in NEEDED {
                if !map.contains_key(*k) {
                    return;
                }
            }
            page.pairing.replace(true);
            page.web_hint.set_text("Signed in — pairing with your phone…");
            let map2 = map;
            crate::bg::run(
                move || -> Result<serde_json::Value, String> {
                    let mut client = nyx_messages_proto::Client::connect_or_start().map_err(|e| e.to_string())?;
                    client.pair_start(map2, true).map_err(|e| e.to_string())
                },
                {
                    let page = page.clone();
                    move |result: Result<serde_json::Value, String>| match result {
                        Ok(v) => {
                            if let Some(emoji) = v.get("emoji").and_then(|x| x.as_str()) {
                                page.show_emoji(emoji);
                            }
                            if v.get("mode").and_then(|x| x.as_str()) == Some("reauthenticated")
                                || v.get("mode").and_then(|x| x.as_str()) == Some("paired")
                            {
                                page.pairing.replace(false);
                                page.status.set_text("");
                                (page.on_paired)();
                            } else {
                                page.web_hint.set_text("Confirm the emoji on your phone…");
                            }
                        }
                        Err(e) => page.show_intro_error(&e),
                    }
                },
            );
        });
    }
}

fn collect_cookies(
    mgr: webkit6::CookieManager,
    uris: Vec<&'static str>,
    mut acc: HashMap<String, String>,
    idx: usize,
    done: impl FnOnce(HashMap<String, String>) + 'static,
) {
    if idx >= uris.len() {
        done(acc);
        return;
    }
    let uri = uris[idx];
    mgr.clone().cookies(uri, None::<&gio::Cancellable>, move |res| {
        if let Ok(list) = res {
            for mut c in list {
                let name = c.name().map(|s| s.to_string()).unwrap_or_default();
                let value = c.value().map(|s| s.to_string()).unwrap_or_default();
                if !name.is_empty() && !value.is_empty() {
                    acc.entry(name).or_insert(value);
                }
            }
        }
        collect_cookies(mgr, uris, acc, idx + 1, done);
    });
}
