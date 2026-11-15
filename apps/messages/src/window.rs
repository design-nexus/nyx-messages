//! Option A · Split inbox: list | conversation, person in the window header.

use crate::model::{self, Bubble, OwnedChat};
use crate::pair::PairPage;
use crate::prefs;
use crate::service;
use crate::settings_dialog;
use crate::widgets;
use gtk::glib;
use gtk::prelude::*;
use nyx_messages_proto::Event;
use nyx_ui::chrome;
use nyx_ui::frame;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "F"], "Search texts"),
    (&["Ctrl", "N"], "New conversation"),
    (&["Ctrl", ","], "Settings"),
    (&["F1"], "Keyboard shortcuts"),
    (&["Ctrl", "Q"], "Quit"),
];

struct Ui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    root_stack: gtk::Stack,
    pair: Rc<PairPage>,
    /// Conversation list — AppFrame sidebar (outside the sheet).
    list_col: gtk::Box,
    list_box: gtk::ListBox,
    /// Top pad that absorbs leftover viewport height so the thread sits on the bottom.
    thread_pad: gtk::Box,
    pad_css: gtk::CssProvider,
    /// Message column (day + bubbles).
    thread: gtk::Box,
    thread_scroll: gtk::ScrolledWindow,
    composer: crate::composer::Composer,
    composer_row: gtk::Box,
    send: gtk::Button,
    typing_row: gtk::Box,
    thread_find_bar: gtk::Box,
    thread_find_entry: gtk::Entry,
    hdr_person: gtk::Box,
    hdr_name: gtk::Label,
    hdr_sub: gtk::Label,
    hdr_face: gtk::Label,
    phone_label: gtk::Label,
    filter: RefCell<String>,
    selected: RefCell<Option<String>>,
    empty: gtk::Box,
    /// Coalesce rapid daemon events so the list isn't rebuilt dozens of times.
    list_refresh_pending: Cell<bool>,
    search_entry: gtk::SearchEntry,
    search_bar: gtk::Box,
    /// Pending local file path for the next send, shown as a chip above the composer.
    pending_attach: RefCell<Option<String>>,
    attach_chip: gtk::Box,
    attach_name: gtk::Label,
    /// "Message Ada" — placeholder when no attachment is pending.
    composer_hint: RefCell<String>,
    /// True while peer is typing in the open chat.
    peer_typing: Cell<bool>,
    typing_debounce: RefCell<Option<glib::SourceId>>,
    /// List search vs in-thread find: "list" | "thread"
    find_scope: Cell<&'static str>,
    thread_find_hits: RefCell<Vec<String>>,
    thread_find_idx: Cell<usize>,
    /// Current bubble width cap (px) and label wrap width (chars); see `sync_thread`.
    bubble_max: Cell<i32>,
    bubble_chars: Cell<i32>,
    /// Height of the top pad that keeps short threads on the bottom.
    pad_h: Cell<i32>,
    /// Chat whose rows are in `thread`, and each row's content signature, so updates
    /// patch only what changed instead of rebuilding the conversation.
    thread_chat: RefCell<Option<String>>,
    row_sigs: RefCell<std::collections::HashMap<String, String>>,
    /// Keep the view pinned to the newest message while content grows.
    stick_bottom: Cell<bool>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

fn current_ui() -> Option<Rc<Ui>> {
    UI.with(|u| u.borrow().clone())
}

pub fn window() -> Option<gtk::ApplicationWindow> {
    current_ui().map(|u| u.window.clone())
}

pub fn overlay() -> Option<gtk::Overlay> {
    current_ui().map(|u| u.overlay.clone())
}

pub fn show_pair_after_unpair() {
    if let Some(u) = current_ui() {
        model::set_paired(false);
        model::replace_chats(&[]);
        u.pair.set_unpair_visible(false);
        u.pair.show_intro_error("Phone unpaired.");
        show_pair_page(&u);
        u.phone_label.set_text("Phone");
        show_empty(&u);
        while let Some(child) = u.list_box.first_child() {
            u.list_box.remove(&child);
        }
    }
}

pub fn present(app: &gtk::Application) {
    if current_ui().is_none() {
        build(app);
    }
    if let Some(u) = current_ui() {
        u.window.present();
    }
}

fn build(app: &gtk::Application) {
    crate::theme::install();

    let default_w = std::env::var("NYX_MESSAGES_WIDTH").ok().and_then(|s| s.parse().ok()).unwrap_or(1100);
    let default_h = std::env::var("NYX_MESSAGES_HEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(720);
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Messages")
        .default_width(default_w)
        .default_height(default_h)
        .build();
    // Half of a 1366 laptop is ~683; stay under that so tiling works.
    window.set_size_request(520, 420);
    window.add_css_class("messages-window");

    let header = chrome::app_header(&window, "messages-chat-symbolic", "Messages");

    let hdr_face = face_tile("", 28);
    hdr_face.add_css_class("sm");
    hdr_face.set_valign(gtk::Align::Center);
    let hdr_name = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(22)
        .css_classes(["messages-hdr-name"])
        .build();
    let hdr_sub =
        gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["messages-hdr-sub"]).build();
    let hdr_text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    hdr_text.set_valign(gtk::Align::Center);
    hdr_text.set_hexpand(false);
    hdr_text.set_halign(gtk::Align::Start);
    hdr_text.append(&hdr_name);
    hdr_text.append(&hdr_sub);
    let hdr_person = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hdr_person.add_css_class("messages-hdr-person");
    hdr_person.set_valign(gtk::Align::Center);
    hdr_person.set_halign(gtk::Align::Start);
    hdr_person.set_hexpand(false);
    hdr_person.set_visible(false);
    hdr_person.append(&hdr_face);
    hdr_person.append(&hdr_text);
    // Left of the header center strip (mockup hdrA), then margin-synced to the
    // chat column so the face lines up with incoming message faces — not packed
    // into the right-hand actions.
    header.center.set_hexpand(true);
    header.center.set_halign(gtk::Align::Fill);
    header.center.append(&hdr_person);
    let search = widgets::icon_button("system-search-symbolic", "Search texts (Ctrl+F)");
    search.set_valign(gtk::Align::Center);
    header.end.append(&search);

    let phone = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    phone.add_css_class("messages-phone-pill");
    phone.set_valign(gtk::Align::Center);
    let phone_ic = gtk::Image::from_icon_name("messages-phone-symbolic");
    phone_ic.set_pixel_size(13);
    phone.append(&phone_ic);
    let phone_label = gtk::Label::new(Some("Phone"));
    phone.append(&phone_label);
    let phone_ok = gtk::Image::from_icon_name("object-select-symbolic");
    phone_ok.add_css_class("ok");
    phone_ok.set_pixel_size(12);
    phone.append(&phone_ok);
    header.end.append(&phone);

    let new_btn = gtk::Button::builder().css_classes(["new-button"]).build();
    new_btn.set_valign(gtk::Align::Center);
    new_btn.set_size_request(-1, 28);
    let new_inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    new_inner.set_valign(gtk::Align::Center);
    new_inner.append(&gtk::Image::from_icon_name("messages-compose-symbolic"));
    let new_lbl = gtk::Label::new(Some("New"));
    new_lbl.add_css_class("new-label");
    new_inner.append(&new_lbl);
    let key = gtk::Label::new(Some("Ctrl N"));
    key.add_css_class("key-hint");
    new_inner.append(&key);
    new_btn.set_child(Some(&new_inner));
    new_btn.set_tooltip_text(Some("New conversation (Ctrl+N)"));
    header.end.append(&new_btn);
    header.end.append(&chrome::settings_button(settings_dialog::open));

    // Chrome title sits beside the icon in the header's left box.
    let app_title = header.start.parent().and_then(|left| {
        let mut child = left.first_child();
        while let Some(w) = child {
            if w.has_css_class("app-title") {
                return w.downcast::<gtk::Label>().ok();
            }
            child = w.next_sibling();
        }
        None
    });
    if app_title.is_none() {
        eprintln!("nyx-messages: could not find app-title for narrow mode");
    }

    // Design 2.0: list lives on the panel (sidebar); only the chat is the sheet.
    let list_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_col.add_css_class("messages-list");
    // Mockup A: 340 wide; narrow path shrinks to 260 / 200.
    list_col.set_size_request(340, -1);
    list_col.set_hexpand(false);
    list_col.set_vexpand(true);
    // Hidden until paired — AppFrame treats a hidden sidebar as no-sidebar.
    list_col.set_visible(false);

    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    tools.add_css_class("messages-list-tools");
    tools.set_valign(gtk::Align::Center);
    let filter0 = prefs::get().filter.clone();
    let segments = widgets::segmented(
        &[("all".into(), "All".into()), ("unread".into(), "Unread".into()), ("starred".into(), "Starred".into())],
        &filter0,
        |id| {
            if let Some(u) = current_ui() {
                *u.filter.borrow_mut() = id;
                prefs::update(|p| p.filter = u.filter.borrow().clone());
                refill_list(&u);
            }
        },
    );
    segments.set_valign(gtk::Align::Center);
    tools.append(&segments);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    tools.append(&spacer);
    list_col.append(&tools);

    let search_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    search_bar.add_css_class("messages-list-tools");
    search_bar.set_visible(false);
    let search_entry = gtk::SearchEntry::builder().placeholder_text("Search conversations").hexpand(true).build();
    search_bar.append(&search_entry);
    list_col.append(&search_bar);

    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .css_classes(["messages-list-scroll"])
        .build();
    let list_box = gtk::ListBox::new();
    // None: we paint .selected ourselves. Browse auto-selects row 0 after any
    // rebuild and fires row_selected, which yanked the open chat to the top.
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.set_activate_on_single_click(true);
    list_box.set_show_separators(false);
    list_box.add_css_class("messages-list-box");
    list_scroll.set_child(Some(&list_box));
    list_col.append(&list_scroll);

    let chat_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    chat_col.add_css_class("messages-chat");
    chat_col.set_hexpand(true);
    chat_col.set_vexpand(true);
    chat_col.set_size_request(0, -1);

    // Mockup `.mchat .scroll { justify-content: flex-end }`: a top pad eats leftover
    // viewport height so short threads rest above the composer; width is forced to the
    // pane so bubble rows pin Start/End.
    let thread_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .hexpand(true)
        .css_classes(["messages-thread-scroll"])
        .build();
    let thread_shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    thread_shell.add_css_class("messages-thread-shell");
    thread_shell.set_hexpand(true);
    thread_shell.set_halign(gtk::Align::Fill);
    // Height via a dedicated CssProvider — empty Box size-requests get collapsed by
    // ScrolledWindow; CSS min-height does not.
    let thread_pad = gtk::Box::new(gtk::Orientation::Vertical, 0);
    thread_pad.add_css_class("messages-thread-pad");
    thread_pad.set_hexpand(true);
    let pad_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &pad_css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 10,
    );
    pad_css.load_from_string(".messages-thread-pad { min-height: 0px; }");
    let thread = gtk::Box::new(gtk::Orientation::Vertical, 8);
    thread.add_css_class("messages-thread");
    thread.set_hexpand(true);
    thread.set_halign(gtk::Align::Fill);
    thread_shell.append(&thread_pad);
    thread_shell.append(&thread);
    // Explicit viewport: the implicit one uses the MINIMUM vscroll policy, which lays
    // every row out at its minimum height (link images squashed, photos collapsed).
    let thread_view = gtk::Viewport::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    thread_view.set_vscroll_policy(gtk::ScrollablePolicy::Natural);
    thread_view.set_scroll_to_focus(false);
    thread_view.set_child(Some(&thread_shell));
    thread_scroll.set_child(Some(&thread_view));
    chat_col.append(&thread_scroll);

    let empty = empty_state();
    empty.set_visible(false);
    chat_col.append(&empty);

    let typing_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    typing_row.add_css_class("messages-typing");
    typing_row.set_halign(gtk::Align::Start);
    typing_row.set_visible(false);
    typing_row.append(&gtk::Label::builder().label("…").css_classes(["messages-typing-dots"]).build());
    chat_col.append(&typing_row);

    let thread_find_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    thread_find_bar.add_css_class("messages-thread-find");
    thread_find_bar.set_visible(false);
    let thread_find_entry = gtk::Entry::builder().placeholder_text("Find in conversation").hexpand(true).build();
    let find_prev = gtk::Button::from_icon_name("go-up-symbolic");
    find_prev.set_tooltip_text(Some("Previous match"));
    let find_next = gtk::Button::from_icon_name("go-down-symbolic");
    find_next.set_tooltip_text(Some("Next match"));
    let find_close = gtk::Button::from_icon_name("window-close-symbolic");
    find_close.set_tooltip_text(Some("Close"));
    thread_find_bar.append(&thread_find_entry);
    thread_find_bar.append(&find_prev);
    thread_find_bar.append(&find_next);
    thread_find_bar.append(&find_close);
    chat_col.append(&thread_find_bar);

    // Pending attachment chip (name + remove), above the composer.
    let attach_chip = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    attach_chip.add_css_class("messages-attach-chip");
    attach_chip.set_halign(gtk::Align::Start);
    attach_chip.set_visible(false);
    let attach_icon = gtk::Image::from_icon_name("messages-attach-symbolic");
    attach_icon.set_pixel_size(14);
    attach_chip.append(&attach_icon);
    let attach_name = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(32)
        .css_classes(["messages-attach-name"])
        .build();
    attach_chip.append(&attach_name);
    let attach_remove = widgets::icon_button("window-close-symbolic", "Remove attachment");
    attach_remove.add_css_class("messages-attach-remove");
    attach_chip.append(&attach_remove);
    chat_col.append(&attach_chip);

    let composer_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    composer_row.add_css_class("messages-composer");
    composer_row.set_valign(gtk::Align::Center);
    let field = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    field.add_css_class("messages-composer-field");
    field.set_hexpand(true);
    field.set_valign(gtk::Align::Center);
    let attach_btn = gtk::Button::from_icon_name("messages-attach-symbolic");
    attach_btn.add_css_class("flat");
    attach_btn.add_css_class("messages-composer-icon");
    attach_btn.set_tooltip_text(Some("Attach file"));
    attach_btn.set_valign(gtk::Align::Center);
    attach_btn.set_hexpand(false);
    field.append(&attach_btn);
    let composer = crate::composer::Composer::new();
    composer.set_placeholder_text(Some("Message"));
    field.append(&composer.root);
    let smile_btn = gtk::Button::from_icon_name("messages-smile-symbolic");
    smile_btn.add_css_class("flat");
    smile_btn.add_css_class("messages-composer-icon");
    smile_btn.set_tooltip_text(Some("Emoji"));
    smile_btn.set_valign(gtk::Align::Center);
    smile_btn.set_hexpand(false);
    field.append(&smile_btn);
    composer_row.append(&field);
    let send = gtk::Button::from_icon_name("messages-send-symbolic");
    send.add_css_class("messages-send");
    send.set_valign(gtk::Align::Center);
    send.set_tooltip_text(Some("Send (Enter · Shift+Enter for a new line)"));
    send.set_sensitive(false);
    composer_row.append(&send);
    chat_col.append(&composer_row);

    let root_stack = gtk::Stack::new();
    root_stack.set_hexpand(true);
    root_stack.set_vexpand(true);
    let pair = PairPage::new({
        move || {
            if let Some(u) = current_ui() {
                enter_inbox(&u);
            }
        }
    });
    root_stack.add_named(&pair.root, Some("pair"));
    root_stack.add_named(&chat_col, Some("inbox"));
    root_stack.set_visible_child_name("pair");

    let frame = frame::AppFrame::new(&header.root);
    // List on the panel; sheet border wraps the chat (or pair) only.
    frame.set_sidebar(&list_col);
    frame.set_flush(true);
    frame.set_page(&root_stack);
    let overlay = frame.install(&window);

    let ui = Rc::new(Ui {
        window: window.clone(),
        overlay,
        root_stack: root_stack.clone(),
        pair: pair.clone(),
        list_col: list_col.clone(),
        list_box: list_box.clone(),
        thread_pad: thread_pad.clone(),
        pad_css: pad_css.clone(),
        thread: thread.clone(),
        thread_scroll: thread_scroll.clone(),
        composer: composer.clone(),
        composer_row: composer_row.clone(),
        send: send.clone(),
        typing_row: typing_row.clone(),
        thread_find_bar: thread_find_bar.clone(),
        thread_find_entry: thread_find_entry.clone(),
        hdr_person: hdr_person.clone(),
        hdr_name,
        hdr_sub,
        hdr_face,
        phone_label: phone_label.clone(),
        filter: RefCell::new(filter0),
        selected: RefCell::new(None),
        empty,
        list_refresh_pending: Cell::new(false),
        search_entry: search_entry.clone(),
        search_bar: search_bar.clone(),
        pending_attach: RefCell::new(None),
        attach_chip: attach_chip.clone(),
        attach_name: attach_name.clone(),
        composer_hint: RefCell::new("Message".into()),
        peer_typing: Cell::new(false),
        typing_debounce: RefCell::new(None),
        find_scope: Cell::new("list"),
        thread_find_hits: RefCell::new(Vec::new()),
        thread_find_idx: Cell::new(0),
        bubble_max: Cell::new(280),
        bubble_chars: Cell::new(36),
        pad_h: Cell::new(-1),
        thread_chat: RefCell::new(None),
        row_sigs: RefCell::new(std::collections::HashMap::new()),
        stick_bottom: Cell::new(true),
    });
    UI.with(|slot| *slot.borrow_mut() = Some(ui.clone()));

    search.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| {
            let show = !ui.search_bar.is_visible();
            ui.search_bar.set_visible(show);
            if show {
                ui.search_entry.grab_focus();
            } else {
                ui.search_entry.set_text("");
                refill_list(&ui);
            }
        }
    ));
    search_entry.connect_search_changed(glib::clone!(
        #[strong]
        ui,
        move |_| refill_list(&ui)
    ));

    thread_scroll.hadjustment().connect_notify_local(Some("page-size"), |_, _| arm_sync());
    {
        let adj = thread_scroll.vadjustment();
        adj.connect_notify_local(Some("page-size"), |_, _| arm_sync());
        // Content grew (new row, link card, photo): refresh the pad and stay pinned.
        // (Deferred: upper changes mid-layout; setting the value there re-queues layout.)
        adj.connect_notify_local(Some("upper"), |_, _| {
            arm_sync();
            glib::idle_add_local_once(|| {
                if let Some(u) = current_ui()
                    && u.stick_bottom.get()
                {
                    let a = u.thread_scroll.vadjustment();
                    a.set_value((a.upper() - a.page_size()).max(0.0));
                }
            });
        });
        adj.connect_value_changed(|a| {
            if let Some(u) = current_ui() {
                u.stick_bottom.set(a.value() >= a.upper() - a.page_size() - 48.0);
            }
        });
    }
    thread_scroll.connect_map(|_| arm_sync());
    // Window width changes (incl. full→half tilts that never touch default-width):
    // the surface's layout signal, not a per-frame tick callback.
    {
        let narrow_on = Rc::new(Cell::new(None::<bool>));
        let list_col = list_col.clone();
        let app_title = app_title.clone();
        let phone_lbl = phone_label.clone();
        let new_lbl = new_lbl.clone();
        let new_key = key.clone();
        let hdr_name = ui.hdr_name.clone();
        let apply = Rc::new(move |win: &gtk::ApplicationWindow, width: i32| {
            if width <= 0 {
                return;
            }
            let narrow = width < 900;
            if narrow_on.get() != Some(narrow) {
                narrow_on.set(Some(narrow));
                if narrow {
                    win.add_css_class("narrow");
                } else {
                    win.remove_css_class("narrow");
                }
                if let Some(t) = &app_title {
                    t.set_visible(!narrow);
                }
                phone_lbl.set_visible(!narrow);
                new_lbl.set_visible(!narrow);
                new_key.set_visible(!narrow);
                hdr_name.set_max_width_chars(if narrow { 10 } else { 22 });
                // Mockup body.narrow .mlist.a { --list-w: 260px }
                list_col.set_size_request(if narrow { 260 } else { 340 }, -1);
            }
            arm_sync();
            after_next_layout(win, align_header);
        });
        window.connect_realize(move |win| {
            let Some(surface) = win.surface() else {
                return;
            };
            apply(win, win.width().max(win.default_width()));
            let win = win.clone();
            let apply = apply.clone();
            // Emitted mid-layout: restyle afterwards, or GTK re-runs layout in a loop.
            surface.connect_layout(move |_, w, _| {
                let win = win.clone();
                let apply = apply.clone();
                glib::idle_add_local_once(move || apply(&win, w));
            });
        });
    }
    // Person face ↔ message faces once the header shows a person.
    ui.hdr_person.connect_map(|w| after_next_layout(w, align_header));

    boot_session(&ui);

    list_box.connect_row_activated(glib::clone!(
        #[strong]
        ui,
        move |_, row| {
            if let Some(id) = row_chat_id(row) {
                select_chat(&ui, &id);
            }
        }
    ));

    composer.connect_changed(glib::clone!(
        #[strong]
        ui,
        move |c| {
            let has_text = !c.text().trim().is_empty();
            let has_attach = ui.pending_attach.borrow().is_some();
            let has = (has_text || has_attach) && ui.selected.borrow().is_some();
            ui.send.set_sensitive(has);
            bump_outbound_typing(ui.clone(), has_text);
        }
    ));
    composer.connect_activate(glib::clone!(
        #[strong]
        ui,
        move || send_draft(&ui)
    ));
    send.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| send_draft(&ui)
    ));
    attach_remove.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| set_pending_attach(&ui, None)
    ));
    // Read state follows what you can see: an open chat in an active window.
    window.connect_is_active_notify(glib::clone!(
        #[strong]
        ui,
        move |_| {
            update_open_chat(&ui);
            let selected = ui.selected.borrow().clone();
            if let Some(id) = selected
                && ui.window.is_active()
                && model::owned_by_id(&id).map(|c| c.unread > 0).unwrap_or(false)
            {
                model::clear_unread(&id);
                clear_row_unread(&ui, &id);
                mark_latest_read(&id, 0);
            }
        }
    ));
    attach_btn.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| pick_attachment(ui.clone())
    ));
    smile_btn.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |btn| open_emoji_chooser(&ui, btn)
    ));
    find_close.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| hide_thread_find(&ui)
    ));
    find_next.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| step_thread_find(&ui, 1)
    ));
    find_prev.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| step_thread_find(&ui, -1)
    ));
    thread_find_entry.connect_activate(glib::clone!(
        #[strong]
        ui,
        move |_| {
            run_thread_find(&ui);
            step_thread_find(&ui, 1);
        }
    ));
    thread_find_entry.connect_changed(glib::clone!(
        #[strong]
        ui,
        move |_| run_thread_find(&ui)
    ));
    new_btn.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| start_new_conversation(&ui)
    ));

    nyx_ui::keys::install(
        &window,
        nyx_ui::keys::Common {
            shortcuts: Some(show_shortcuts),
            settings: Some(settings_dialog::open),
            close: true,
            ..Default::default()
        },
    );
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[strong]
        ui,
        move |_, key, _, mods| {
            let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            match key {
                gtk::gdk::Key::f | gtk::gdk::Key::F if ctrl => {
                    if ui.selected.borrow().is_some() && ui.thread_scroll.is_visible() {
                        show_thread_find(&ui);
                    } else {
                        ui.find_scope.set("list");
                        ui.search_bar.set_visible(true);
                        ui.search_entry.grab_focus();
                    }
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::n | gtk::gdk::Key::N if ctrl => {
                    start_new_conversation(&ui);
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::comma if ctrl => {
                    settings_dialog::open();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Escape if settings_dialog::is_open() => {
                    settings_dialog::escape();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Escape if ui.thread_find_bar.is_visible() => {
                    hide_thread_find(&ui);
                    ui.composer.grab_focus();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Escape if ui.search_bar.is_visible() => {
                    ui.search_bar.set_visible(false);
                    ui.search_entry.set_text("");
                    refill_list(&ui);
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::F1 => {
                    show_shortcuts();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    window.add_controller(keys);

    window.connect_close_request(|_| {
        UI.with(|slot| *slot.borrow_mut() = None);
        glib::Propagation::Proceed
    });
}

fn show_shortcuts() {
    let (dialog, body) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, action) in SHORTCUTS {
        list.append(&widgets::row(action, "", Some(widgets::key_caps(keys).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(480)
        .child(&list)
        .build();
    body.append(&scroll);
    // Escape or the window's close button dismisses it (widgets::dialog).
    dialog.present();
}

fn send_draft(ui: &Ui) {
    let text = ui.composer.text().trim().to_string();
    let attach = ui.pending_attach.borrow().clone();
    if text.is_empty() && attach.is_none() {
        return;
    }
    let Some(id) = ui.selected.borrow().clone() else {
        return;
    };
    clear_outbound_typing(ui);
    ui.composer.set_text("");
    set_pending_attach(ui, None);
    send_message(ui, &id, text, attach);
}

/// Optimistic bubble now; the daemon's echo (or a refetch) replaces it.
fn send_message(ui: &Ui, chat_id: &str, text: String, attach: Option<String>) {
    let local_id = model::new_local_id();
    let file_chip = attach.as_ref().map(|p| {
        let name = std::path::Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or("Attachment").to_string();
        (name, "Sending…".into())
    });
    model::push_outgoing_optimistic(chat_id, &local_id, &text, file_chip, attach.clone());
    if let Some(chat) = model::owned_by_id(chat_id) {
        fill_thread_owned(ui, &chat);
        scroll_thread_to_end(ui);
        sync_list(ui);
    }
    let id = chat_id.to_string();
    let id2 = id.clone();
    crate::bg::run(
        move || match attach.as_deref() {
            Some(path) => service::send_media(&id, &text, path).map_err(|e| e.to_string()),
            None => service::send_text(&id, &text).map_err(|e| e.to_string()),
        },
        move |result| match result {
            Ok(daemon_tmp) => {
                model::adopt_tmp_id(&id2, &local_id, &daemon_tmp);
                let id = id2.clone();
                crate::bg::run(
                    move || service::fetch_messages(&id).map_err(|e| e.to_string()),
                    move |msgs| {
                        if let Ok(msgs) = msgs {
                            model::replace_messages(&id2, &msgs);
                        }
                        refresh_open_thread(&id2);
                    },
                );
            }
            Err(e) => {
                eprintln!("nyx-messages: send failed: {e}");
                model::mark_bubble_failed(&id2, &local_id);
                refresh_open_thread(&id2);
            }
        },
    );
}

/// Re-sync the thread if `chat_id` is the open conversation.
fn refresh_open_thread(chat_id: &str) {
    if let Some(u) = current_ui()
        && u.selected.borrow().as_deref() == Some(chat_id)
        && let Some(chat) = model::owned_by_id(chat_id)
    {
        fill_thread_owned(&u, &chat);
        refresh_send_sensitive(&u);
    }
}

fn retry_failed(msg_id: &str) {
    let Some(ui) = current_ui() else {
        return;
    };
    let Some(chat_id) = ui.selected.borrow().clone() else {
        return;
    };
    if let Some(b) = model::remove_bubble(&chat_id, msg_id) {
        send_message(&ui, &chat_id, b.body, b.local_path);
    }
}

fn delete_failed(msg_id: &str) {
    let Some(ui) = current_ui() else {
        return;
    };
    let Some(chat_id) = ui.selected.borrow().clone() else {
        return;
    };
    model::remove_bubble(&chat_id, msg_id);
    refresh_open_thread(&chat_id);
}

/// Pending attachment: a removable chip above the composer, caption placeholder.
fn set_pending_attach(ui: &Ui, path: Option<String>) {
    match &path {
        Some(p) => {
            let name = std::path::Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
            ui.attach_name.set_text(&name);
            ui.attach_chip.set_visible(true);
            ui.composer.set_placeholder_text(Some(&format!("Caption for {name} (optional)")));
        }
        None => {
            ui.attach_chip.set_visible(false);
            ui.composer.set_placeholder_text(Some(ui.composer_hint.borrow().as_str()));
        }
    }
    *ui.pending_attach.borrow_mut() = path;
    refresh_send_sensitive(ui);
}

fn refresh_send_sensitive(ui: &Ui) {
    let has = (!ui.composer.text().trim().is_empty() || ui.pending_attach.borrow().is_some()) && ui.selected.borrow().is_some();
    ui.send.set_sensitive(has);
}

fn pick_attachment(ui: Rc<Ui>) {
    let dialog = gtk::FileDialog::builder().title("Attach file").modal(true).build();
    let win = ui.window.clone();
    dialog.open(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res
            && let Some(path) = file.path()
        {
            set_pending_attach(&ui, Some(path.display().to_string()));
        }
    });
}

fn open_emoji_chooser(ui: &Ui, btn: &gtk::Button) {
    let chooser = gtk::EmojiChooser::new();
    chooser.set_parent(btn);
    let composer = ui.composer.clone();
    chooser.connect_emoji_picked(move |c, emoji| {
        composer.insert_at_cursor(emoji);
        c.popdown();
    });
    // One chooser per open; drop it when it closes instead of piling them up.
    chooser.connect_closed(|c| {
        let c = c.clone();
        glib::idle_add_local_once(move || c.unparent());
    });
    chooser.popup();
}

fn bump_outbound_typing(ui: Rc<Ui>, has_text: bool) {
    let Some(chat_id) = ui.selected.borrow().clone() else {
        return;
    };
    if !has_text {
        clear_outbound_typing(&ui);
        return;
    }
    // Already told the phone we're typing: just push the stop timer back.
    let active = ui.typing_debounce.borrow_mut().take();
    match active {
        Some(id) => id.remove(),
        None => {
            let chat = chat_id.clone();
            crate::bg::run(move || service::typing_set(&chat, true).map_err(|e| e.to_string()), |_| {});
        }
    }
    let ui2 = ui.clone();
    let id = glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
        *ui2.typing_debounce.borrow_mut() = None;
        crate::bg::run(move || service::typing_set(&chat_id, false).map_err(|e| e.to_string()), |_| {});
    });
    *ui.typing_debounce.borrow_mut() = Some(id);
}

fn clear_outbound_typing(ui: &Ui) {
    let Some(id) = ui.typing_debounce.borrow_mut().take() else {
        return; // never said we were typing
    };
    id.remove();
    if let Some(chat_id) = ui.selected.borrow().clone() {
        crate::bg::run(move || service::typing_set(&chat_id, false).map_err(|e| e.to_string()), |_| {});
    }
}

fn show_thread_find(ui: &Ui) {
    ui.find_scope.set("thread");
    ui.thread_find_bar.set_visible(true);
    ui.thread_find_entry.grab_focus();
}

fn hide_thread_find(ui: &Ui) {
    ui.thread_find_bar.set_visible(false);
    ui.thread_find_hits.borrow_mut().clear();
    ui.thread_find_idx.set(0);
    clear_thread_find_highlights(ui);
}

fn run_thread_find(ui: &Ui) {
    let q = ui.thread_find_entry.text().to_ascii_lowercase();
    clear_thread_find_highlights(ui);
    let mut hits = Vec::new();
    if q.is_empty() {
        *ui.thread_find_hits.borrow_mut() = hits;
        return;
    }
    let Some(chat_id) = ui.selected.borrow().clone() else {
        return;
    };
    for b in model::bubbles_for_owned(&chat_id) {
        if b.body.to_ascii_lowercase().contains(&q) {
            hits.push(b.id.clone());
        }
    }
    *ui.thread_find_hits.borrow_mut() = hits;
    ui.thread_find_idx.set(usize::MAX);
    highlight_thread_find_hits(ui);
}

fn step_thread_find(ui: &Ui, delta: i32) {
    let hits = ui.thread_find_hits.borrow().clone();
    if hits.is_empty() {
        return;
    }
    let n = hits.len();
    let cur = ui.thread_find_idx.get();
    let next = if cur == usize::MAX {
        if delta >= 0 { 0 } else { n - 1 }
    } else if delta >= 0 {
        (cur + 1) % n
    } else {
        cur.checked_sub(1).unwrap_or(n - 1)
    };
    ui.thread_find_idx.set(next);
    highlight_thread_find_hits(ui);
    scroll_to_bubble(ui, &hits[next]);
}

fn clear_thread_find_highlights(ui: &Ui) {
    let mut row = ui.thread.first_child();
    while let Some(w) = row {
        row = w.next_sibling();
        w.remove_css_class("messages-find-hit");
        w.remove_css_class("messages-find-current");
    }
}

fn highlight_thread_find_hits(ui: &Ui) {
    let hits = ui.thread_find_hits.borrow().clone();
    let cur_id = hits.get(ui.thread_find_idx.get()).cloned();
    let mut row = ui.thread.first_child();
    while let Some(w) = row {
        row = w.next_sibling();
        w.remove_css_class("messages-find-hit");
        w.remove_css_class("messages-find-current");
        if let Some(id) = widget_bubble_id(&w) {
            if hits.iter().any(|h| h == &id) {
                w.add_css_class("messages-find-hit");
            }
            if cur_id.as_deref() == Some(id.as_str()) {
                w.add_css_class("messages-find-current");
            }
        }
    }
}

fn widget_bubble_id(w: &gtk::Widget) -> Option<String> {
    // Store id on the row via widget name.
    let name = w.widget_name();
    if name.starts_with("bubble:") { Some(name.trim_start_matches("bubble:").to_string()) } else { None }
}

fn scroll_to_bubble(ui: &Ui, id: &str) {
    let name = format!("bubble:{id}");
    let Some(content) = ui.thread.parent() else {
        return;
    };
    let mut row = ui.thread.first_child();
    while let Some(w) = row {
        row = w.next_sibling();
        if w.widget_name() != name.as_str() {
            continue;
        }
        if let Some(b) = w.compute_bounds(&content) {
            let adj = ui.thread_scroll.vadjustment();
            // Centre the hit a little above the middle; stop following the bottom.
            ui.stick_bottom.set(false);
            let y = b.y() as f64 - (adj.page_size() - b.height() as f64) / 3.0;
            adj.set_value(y.clamp(0.0, (adj.upper() - adj.page_size()).max(0.0)));
        }
        return;
    }
}

fn set_peer_typing(ui: &Ui, chat_id: &str, typing: bool) {
    if ui.selected.borrow().as_deref() != Some(chat_id) {
        return;
    }
    ui.peer_typing.set(typing);
    ui.typing_row.set_visible(typing);
}

fn start_new_conversation(_ui: &Ui) {
    if current_ui().is_none() {
        return;
    }
    // Shared Nyx dialog card (Escape / close cancel; Enter or the arrow starts).
    let (d, body) = widgets::dialog("New conversation", 360);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entry = gtk::Entry::builder()
        .placeholder_text("Phone number, e.g. +1 555 0100")
        .input_purpose(gtk::InputPurpose::Phone)
        .hexpand(true)
        .build();
    row.append(&entry);
    let start = widgets::icon_button("go-next-symbolic", "Start conversation");
    start.add_css_class("suggested-action");
    start.set_valign(gtk::Align::Center);
    row.append(&start);
    body.append(&row);
    let err = gtk::Label::builder().label("").xalign(0.0).wrap(true).visible(false).css_classes(["error"]).build();
    body.append(&err);
    let try_start = {
        let d = d.clone();
        let entry = entry.clone();
        let start = start.clone();
        let err = err.clone();
        move || {
            let num = entry.text().trim().to_string();
            if num.is_empty() {
                err.set_text("Enter a phone number.");
                err.set_visible(true);
                return;
            }
            // Prefer an already-loaded local match (mock or live cache).
            if let Some(chat) = model::find_chat_by_phone(&num)
                && let Some(u) = current_ui()
            {
                select_chat(&u, &chat.id);
                u.composer.grab_focus();
                d.close();
                return;
            }
            if std::env::var_os("NYX_MESSAGES_MOCK").is_some() {
                err.set_text("No matching chat in mock mode.");
                err.set_visible(true);
                return;
            }
            start.set_sensitive(false);
            err.set_visible(false);
            let phone = num.clone();
            let d2 = d.clone();
            let start2 = start.clone();
            let err2 = err.clone();
            crate::bg::run(
                move || service::get_or_create_chat(&phone).map_err(|e| e.to_string()),
                move |result| match result {
                    Ok(chat) => {
                        model::upsert_chat(&chat);
                        if let Some(u) = current_ui() {
                            refill_list(&u);
                            select_chat(&u, &chat.id);
                            u.composer.grab_focus();
                        }
                        d2.close();
                    }
                    Err(e) => {
                        err2.set_text(&format!("Couldn't start chat: {e}"));
                        err2.set_visible(true);
                        start2.set_sensitive(true);
                    }
                },
            );
        }
    };
    start.connect_clicked({
        let try_start = try_start.clone();
        move |_| try_start()
    });
    entry.connect_activate({
        let try_start = try_start.clone();
        move |_| try_start()
    });
    d.present();
    entry.grab_focus();
}

fn row_chat_id(row: &gtk::ListBoxRow) -> Option<String> {
    row.widget_name().strip_prefix("chat:").map(str::to_string)
}

fn boot_session(ui: &Rc<Ui>) {
    if std::env::var_os("NYX_MESSAGES_MOCK").is_some() {
        model::set_paired(true);
        ui.phone_label.set_text("Phone");
        ui.pair.set_unpair_visible(false);
        enter_inbox(ui);
        return;
    }

    let (evt_tx, evt_rx) = async_channel::unbounded::<Event>();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(evt) = evt_rx.recv().await {
            if let Some(u) = current_ui() {
                on_daemon_event(&u, evt);
            }
        }
    });
    service::spawn_event_thread(evt_tx);

    let ui_boot = ui.clone();
    crate::bg::run(
        || service::refresh_status().map_err(|e| e.to_string()),
        move |status| match status {
            Ok(s) if s.paired => {
                model::set_paired(true);
                ui_boot.phone_label.set_text(if s.account.is_empty() {
                    "Phone"
                } else {
                    s.account.split('@').next().unwrap_or("Phone")
                });
                ui_boot.pair.set_unpair_visible(true);
                enter_inbox(&ui_boot);
            }
            Ok(_) => {
                model::set_paired(false);
                show_pair_page(&ui_boot);
            }
            Err(e) => {
                ui_boot.pair.show_intro_error(&format!("Daemon: {e}"));
                show_pair_page(&ui_boot);
            }
        },
    );
}

fn show_pair_page(ui: &Ui) {
    ui.list_col.set_visible(false);
    ui.root_stack.set_visible_child_name("pair");
}

fn enter_inbox(ui: &Ui) {
    ui.list_col.set_visible(true);
    ui.root_stack.set_visible_child_name("inbox");
    if std::env::var_os("NYX_MESSAGES_MOCK").is_some() {
        paint_inbox(ui);
        return;
    }
    crate::bg::run(
        || service::fetch_chats().map_err(|e| e.to_string()),
        move |result| {
            if let Some(u) = current_ui() {
                match result {
                    Ok(chats) => {
                        eprintln!("nyx-messages: fetched {} chats", chats.len());
                        model::replace_chats(&chats);
                    }
                    Err(e) => {
                        eprintln!("nyx-messages: load chats: {e}");
                        u.empty.set_visible(true);
                        if let Some(label) = u.empty.first_child().and_then(|c| c.next_sibling()).and_downcast::<gtk::Label>() {
                            label.set_text(&format!("Couldn't load chats: {e}"));
                        }
                    }
                }
                paint_inbox(&u);
            }
        },
    );
}

fn paint_inbox(ui: &Ui) {
    let filter = ui.filter.borrow().clone();
    let mut chats = visible_chats(ui, &filter);
    // Don't strand the user on an empty Unread/Starred filter after pair.
    if chats.is_empty() && filter != "all" && !model::owned_chats().is_empty() {
        *ui.filter.borrow_mut() = "all".into();
        prefs::update(|p| p.filter = "all".into());
        chats = visible_chats(ui, "all");
    }
    eprintln!("nyx-messages: paint_inbox {} chats (filter={})", chats.len(), ui.filter.borrow());
    refill_list(ui);
    if let Some(first) = chats.into_iter().next() {
        select_chat(ui, &first.id);
    } else {
        show_empty(ui);
    }
}

fn visible_chats(ui: &Ui, filter: &str) -> Vec<OwnedChat> {
    let q = ui.search_entry.text().to_ascii_lowercase();
    model::filter_owned(filter)
        .into_iter()
        .filter(|c| q.is_empty() || c.name.to_ascii_lowercase().contains(&q) || c.preview.to_ascii_lowercase().contains(&q))
        .collect()
}

fn schedule_list_refresh() {
    let Some(ui) = current_ui() else {
        return;
    };
    if ui.list_refresh_pending.get() {
        return;
    }
    ui.list_refresh_pending.set(true);
    glib::timeout_add_local_once(std::time::Duration::from_millis(120), || {
        if let Some(u) = current_ui() {
            u.list_refresh_pending.set(false);
            // Prefer in-place updates so hover/selection don't flicker.
            sync_list(&u);
        }
    });
}

fn on_daemon_event(ui: &Ui, evt: Event) {
    service::apply_event(&evt);
    match &evt {
        Event::PairEmoji { emoji, .. } => {
            ui.pair.show_emoji(emoji);
            show_pair_page(ui);
        }
        Event::PairOk { account, .. } => {
            if !account.is_empty() {
                ui.phone_label.set_text(account.split('@').next().unwrap_or("Phone"));
            }
            ui.pair.set_unpair_visible(true);
            enter_inbox(ui);
        }
        Event::PairFail { error } => {
            ui.pair.show_intro_error(if error.is_empty() { "Pairing failed" } else { error });
            show_pair_page(ui);
        }
        Event::ChatsRefresh => {
            crate::bg::run(
                || service::fetch_chats().map_err(|e| e.to_string()),
                move |result| {
                    if let Some(u) = current_ui() {
                        match result {
                            Ok(chats) => model::replace_chats(&chats),
                            Err(e) => eprintln!("nyx-messages: refresh chats: {e}"),
                        }
                        // Keep the open chat — never paint_inbox (that selects first).
                        sync_list(&u);
                    }
                },
            );
        }
        Event::ChatUpsert { .. } => schedule_list_refresh(),
        Event::MessageUpsert { message } => {
            let selected = ui.selected.borrow().clone();
            if selected.as_deref() == Some(message.chat_id.as_str())
                && let Some(chat) = model::owned_by_id(&message.chat_id)
            {
                fill_thread_owned(ui, &chat);
                // Reading history: don't yank the view to the bottom.
                if message.from_me || ui.stick_bottom.get() {
                    scroll_thread_to_end(ui);
                }
                if !message.from_me && ui.window.is_active() {
                    mark_latest_read(&message.chat_id, 0);
                }
            }
            schedule_list_refresh();
        }
        Event::Connection { state, .. } => {
            if state == "needs_repair" {
                ui.phone_label.set_text("Repair");
            } else if state == "connected" {
                // keep account label
            } else if state == "connecting" {
                ui.phone_label.set_text("…");
            }
        }
        Event::NeedsRepair { .. } => {
            ui.phone_label.set_text("Repair");
            show_pair_page(ui);
            ui.pair.show_intro_error("Session needs repair — sign in again.");
        }
        Event::Typing { chat_id, typing } => {
            set_peer_typing(ui, chat_id, *typing);
        }
    }
}

fn find_list_row(ui: &Ui, chat_id: &str) -> Option<gtk::ListBoxRow> {
    let mut child = ui.list_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        if row_chat_id(&row).as_deref() == Some(chat_id) {
            return Some(row);
        }
    }
    None
}

fn row_index_in_list(ui: &Ui, target: &gtk::ListBoxRow) -> Option<i32> {
    let mut i = 0i32;
    let mut child = ui.list_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        if row == *target {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Patch labels / reorder existing rows — never destroy the whole list.
/// That was flickering hover and yanking selection to the top chat.
fn sync_list(ui: &Ui) {
    let filter = ui.filter.borrow().clone();
    let selected = ui.selected.borrow().clone();
    let chats = visible_chats(ui, &filter);
    if chats.is_empty() {
        while let Some(child) = ui.list_box.first_child() {
            ui.list_box.remove(&child);
        }
        if model::owned_chats().is_empty() {
            show_empty(ui);
        }
        return;
    }
    ui.empty.set_visible(false);

    let want: std::collections::HashSet<String> = chats.iter().map(|c| c.id.clone()).collect();
    // Drop rows that are gone / filtered out.
    let mut child = ui.list_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        match row_chat_id(&row) {
            Some(id) if want.contains(&id) => {}
            _ => ui.list_box.remove(&row),
        }
    }

    for (i, chat) in chats.iter().enumerate() {
        let i = i as i32;
        if let Some(row) = find_list_row(ui, &chat.id) {
            patch_list_row_widget(&row, chat);
            if row_index_in_list(ui, &row) != Some(i) {
                ui.list_box.remove(&row);
                ui.list_box.insert(&row, i);
            }
        } else {
            let row = chat_row(chat);
            ui.list_box.insert(&row, i);
        }
    }

    // Refresh .selected CSS without changing the open chat.
    let mut child = ui.list_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let on = selected.as_deref() == row_chat_id(&row).as_deref();
        if let Some(inner) = row.child() {
            if on {
                inner.add_css_class("selected");
            } else {
                inner.remove_css_class("selected");
            }
        }
    }
}

fn patch_list_row_widget(row: &gtk::ListBoxRow, chat: &OwnedChat) {
    let Some(inner) = row.child() else {
        return;
    };
    // crow: face | tx(r1: name…when, r2: preview…badge?)
    let Some(tx) = inner.last_child() else {
        return;
    };
    if let Some(r1) = tx.first_child()
        && let Some(when) = r1.last_child()
        && when.has_css_class("messages-crow-when")
        && let Ok(label) = when.downcast::<gtk::Label>()
    {
        label.set_text(&chat.when);
    }
    if let Some(r2w) = tx.last_child() {
        if let Some(preview) = r2w.first_child()
            && preview.has_css_class("messages-crow-preview")
            && let Ok(label) = preview.downcast::<gtk::Label>()
        {
            label.set_text(&chat.preview);
        }
        if let Ok(r2) = r2w.downcast::<gtk::Box>() {
            let mut n = r2.first_child();
            while let Some(w) = n {
                n = w.next_sibling();
                if w.has_css_class("messages-unread") {
                    r2.remove(&w);
                }
            }
            if chat.unread > 0 {
                let badge = gtk::Label::new(Some(&chat.unread.to_string()));
                badge.add_css_class("messages-unread");
                badge.set_valign(gtk::Align::Center);
                r2.append(&badge);
            }
        }
    }
}

fn clear_row_unread(ui: &Ui, chat_id: &str) {
    let mut child = ui.list_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        if row_chat_id(&row).as_deref() != Some(chat_id) {
            continue;
        }
        let Some(inner) = row.child() else {
            return;
        };
        let Some(tx) = inner.last_child() else {
            return;
        };
        let Some(r2w) = tx.last_child() else {
            return;
        };
        let Ok(r2) = r2w.downcast::<gtk::Box>() else {
            return;
        };
        let mut n = r2.first_child();
        while let Some(w) = n {
            n = w.next_sibling();
            if w.has_css_class("messages-unread") {
                r2.remove(&w);
            }
        }
        return;
    }
}

fn refill_list(ui: &Ui) {
    let selected = ui.selected.borrow().clone();
    while let Some(child) = ui.list_box.first_child() {
        ui.list_box.remove(&child);
    }
    let filter = ui.filter.borrow().clone();
    let chats = visible_chats(ui, &filter);
    if chats.is_empty() {
        if model::owned_chats().is_empty() {
            show_empty(ui);
        }
        return;
    }
    ui.empty.set_visible(false);
    for chat in &chats {
        let row = chat_row(chat);
        if selected.as_deref() == Some(chat.id.as_str())
            && let Some(inner) = row.child()
        {
            inner.add_css_class("selected");
        }
        ui.list_box.append(&row);
    }
}

fn chat_row(chat: &OwnedChat) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&format!("chat:{}", chat.id));
    // Even rail: face | name…………when / preview………badge — no optional RCS/check
    // chips that shove text around from row to row.
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 11);
    box_.add_css_class("messages-crow");
    box_.set_hexpand(true);
    box_.set_valign(gtk::Align::Center);
    let face = face_tile(&chat.name, 40);
    face.set_valign(gtk::Align::Center);
    box_.append(&face);

    let tx = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tx.set_hexpand(true);
    tx.set_halign(gtk::Align::Fill);
    tx.set_valign(gtk::Align::Center);

    let r1 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    r1.set_hexpand(true);
    r1.set_valign(gtk::Align::Center);
    let name = gtk::Label::builder()
        .label(&chat.name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(["messages-crow-name"])
        .build();
    r1.append(&name);
    if chat.starred {
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.add_css_class("messages-crow-meta");
        star.add_css_class("st");
        star.set_pixel_size(13);
        star.set_valign(gtk::Align::Center);
        r1.append(&star);
    }
    let when =
        gtk::Label::builder().label(&chat.when).xalign(1.0).single_line_mode(true).css_classes(["messages-crow-when"]).build();
    when.set_valign(gtk::Align::Center);
    r1.append(&when);
    tx.append(&r1);

    let r2 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    r2.set_hexpand(true);
    r2.set_valign(gtk::Align::Center);
    let preview = gtk::Label::builder()
        .label(&chat.preview)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(["messages-crow-preview"])
        .build();
    r2.append(&preview);
    if chat.unread > 0 {
        let badge = gtk::Label::new(Some(&chat.unread.to_string()));
        badge.add_css_class("messages-unread");
        badge.set_valign(gtk::Align::Center);
        r2.append(&badge);
    }
    tx.append(&r2);
    box_.append(&tx);
    row.set_child(Some(&box_));
    row
}

fn show_empty(ui: &Ui) {
    *ui.selected.borrow_mut() = None;
    model::set_open_chat(None);
    ui.hdr_person.set_visible(false);
    ui.thread_scroll.set_visible(false);
    ui.composer_row.set_visible(false);
    ui.typing_row.set_visible(false);
    hide_thread_find(ui);
    ui.empty.set_visible(true);
}

fn select_chat(ui: &Ui, id: &str) {
    let id = id.to_string();
    let already = ui.selected.borrow().as_deref() == Some(id.as_str());
    if !already {
        // "Stopped typing" goes to the chat we were typing in.
        clear_outbound_typing(ui);
    }
    *ui.selected.borrow_mut() = Some(id.clone());
    let prev_unread = model::owned_by_id(&id).map(|c| c.unread).unwrap_or(0);
    model::clear_unread(&id);
    // Clear the badge in place — never rebuild the list here.
    clear_row_unread(ui, &id);
    ui.peer_typing.set(false);
    ui.typing_row.set_visible(false);
    // Mark list selection CSS.
    let mut child = ui.list_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let on = row_chat_id(&row).as_deref() == Some(id.as_str());
        if let Some(inner) = row.child() {
            if on {
                inner.add_css_class("selected");
            } else {
                inner.remove_css_class("selected");
            }
        }
    }
    update_open_chat(ui);
    if already {
        return;
    }
    // An attachment picked in another chat must never go to this one.
    if ui.pending_attach.borrow().is_some() {
        set_pending_attach(ui, None);
    }
    if let Some(chat) = model::owned_by_id(&id) {
        paint_chat(ui, &chat);
    }
    if std::env::var_os("NYX_MESSAGES_MOCK").is_some() {
        return;
    }
    let id2 = id.clone();
    crate::bg::run(
        move || service::fetch_messages(&id).map_err(|e| e.to_string()),
        move |result| {
            if let Some(u) = current_ui()
                && u.selected.borrow().as_deref() == Some(id2.as_str())
            {
                match result {
                    Ok(msgs) => model::replace_messages(&id2, &msgs),
                    Err(e) => eprintln!("nyx-messages: load messages: {e}"),
                }
                if let Some(chat) = model::owned_by_id(&id2) {
                    paint_chat(&u, &chat);
                }
                // Mark read on the phone with the latest inbound id once we have messages.
                mark_latest_read(&id2, prev_unread);
            }
        },
    );
}

fn paint_chat(ui: &Ui, chat: &OwnedChat) {
    set_face(&ui.hdr_face, &chat.name);
    ui.hdr_name.set_text(&chat.name);
    ui.hdr_sub.set_text(&chat.subtitle);
    ui.hdr_sub.set_visible(!chat.subtitle.is_empty());
    ui.hdr_person.set_visible(true);
    let first = chat.name.split_whitespace().next().unwrap_or(chat.name.as_str());
    *ui.composer_hint.borrow_mut() = format!("Message {first}");
    if ui.pending_attach.borrow().is_none() {
        ui.composer.set_placeholder_text(Some(ui.composer_hint.borrow().as_str()));
    }
    ui.empty.set_visible(false);
    ui.thread_scroll.set_visible(true);
    ui.composer_row.set_visible(true);
    fill_thread_owned(ui, chat);
    scroll_thread_to_end(ui);

    let id = chat.id.as_str();
    let mut child = ui.list_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let on = row_chat_id(&row).as_deref() == Some(id);
        if let Some(inner) = row.child() {
            if on {
                inner.add_css_class("selected");
            } else {
                inner.remove_css_class("selected");
            }
        }
        // Do not call select_row here — that re-enters select_chat and freezes GTK.
    }
}

/// Bring `thread` in line with the model. Rows are keyed by message id and only
/// rebuilt when their content changes, so new messages don't reset scroll, text
/// selection, link cards or downloaded photos on every other row.
fn fill_thread_owned(ui: &Ui, chat: &OwnedChat) {
    if ui.thread_chat.borrow().as_deref() != Some(chat.id.as_str()) {
        while let Some(child) = ui.thread.first_child() {
            ui.thread.remove(&child);
        }
        ui.row_sigs.borrow_mut().clear();
        *ui.thread_chat.borrow_mut() = Some(chat.id.clone());
        ui.stick_bottom.set(true);
    }
    // Build at the current width so rows never start at a stale cap.
    let w = ui.thread_scroll.width();
    if w > 0 {
        let (max_cluster, chars) = bubble_widths(ui, w);
        ui.bubble_max.set(max_cluster);
        ui.bubble_chars.set(chars);
    }
    let (max_cluster, chars) = (ui.bubble_max.get(), ui.bubble_chars.get());

    let mut existing: std::collections::HashMap<String, gtk::Widget> = std::collections::HashMap::new();
    let mut child = ui.thread.first_child();
    while let Some(w) = child {
        child = w.next_sibling();
        existing.insert(w.widget_name().to_string(), w);
    }
    let old_sigs = std::mem::take(&mut *ui.row_sigs.borrow_mut());
    let mut new_sigs = std::collections::HashMap::new();
    let mut prev: Option<gtk::Widget> = None;
    let mut place = |key: String, sig: String, build: &dyn Fn() -> gtk::Widget| {
        let w = match existing.remove(&key) {
            Some(w) if old_sigs.get(&key) == Some(&sig) => w,
            Some(old) => {
                ui.thread.remove(&old);
                build()
            }
            None => build(),
        };
        w.set_widget_name(&key);
        if w.parent().is_none() {
            ui.thread.insert_child_after(&w, prev.as_ref());
        } else if w.prev_sibling() != prev {
            ui.thread.reorder_child_after(&w, prev.as_ref());
        }
        new_sigs.insert(key, sig);
        prev = Some(w);
    };

    let mut last_day = 0i32;
    let mut last_from_me: Option<bool> = None;
    for (i, b) in model::bubbles_for_owned(&chat.id).iter().enumerate() {
        let key = model::day_key(b.timestamp_ms);
        if key != 0 && key != last_day {
            last_day = key;
            last_from_me = None;
            let text = model::format_day(b.timestamp_ms);
            place(format!("day:{key}"), text.clone(), &|| {
                let day = gtk::Label::new(Some(&text));
                day.add_css_class("messages-day");
                day.set_halign(gtk::Align::Center);
                day.upcast()
            });
        }
        // Tighter gap when the same side sends back-to-back.
        let stacked = last_from_me == Some(b.from_me);
        last_from_me = Some(b.from_me);
        let row_key = if b.id.is_empty() { format!("bubble:anon-{i}") } else { format!("bubble:{}", b.id) };
        place(row_key, format!("{stacked}|{b:?}"), &|| {
            let row = bubble_widget(b, max_cluster, chars);
            if stacked {
                row.add_css_class("stacked");
            }
            row.upcast()
        });
    }
    for gone in existing.values() {
        ui.thread.remove(gone);
    }
    *ui.row_sigs.borrow_mut() = new_sigs;
    arm_sync();
}

fn scroll_thread_to_end(ui: &Ui) {
    ui.stick_bottom.set(true);
    let adj = ui.thread_scroll.vadjustment();
    adj.set_value((adj.upper() - adj.page_size()).max(0.0));
}

/// `.messages-bubble` horizontal padding (11px each side, style.css).
const BUBBLE_PAD_X: i32 = 22;

fn set_bubble_max_width(root: &gtk::Widget, max_px: i32, chars: i32) {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if w.has_css_class("messages-bubble-clamp")
            && let Ok(clamp) = w.clone().downcast::<nyx_ui::clamp::Clamp>()
        {
            clamp.set_max(max_px);
        }
        if let Ok(label) = w.clone().downcast::<gtk::Label>()
            && (label.has_css_class("messages-bubble-body")
                || label.has_css_class("messages-link-title")
                || label.has_css_class("messages-link-desc"))
        {
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
            label.set_max_width_chars(chars);
            label.set_hexpand(false);
        }
        // Fixed-width children must fit inside the bubble padding, or the bubble's
        // minimum exceeds the clamp max and its text spills into the next row.
        let inner = (max_px - BUBBLE_PAD_X).max(0);
        if w.has_css_class("messages-link-img") {
            w.set_size_request(inner.clamp(1, 420), 96);
            w.set_hexpand(false);
        }
        if w.has_css_class("messages-att-img") {
            w.set_size_request(inner.min(180), w.size_request().1);
        }
        if w.has_css_class("messages-file") {
            w.set_size_request(inner.min(160), -1);
        }
        if w.has_css_class("messages-bubble") || w.has_css_class("messages-bubble-cluster") {
            w.set_hexpand(false);
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
}

fn bubble_widget(b: &Bubble, max_cluster: i32, chars: i32) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("messages-bubble-row");
    if !b.id.is_empty() {
        row.set_widget_name(&format!("bubble:{}", b.id));
    }
    row.set_hexpand(true);
    if b.from_me {
        row.add_css_class("me");
    }
    if b.status == "failed" {
        row.add_css_class("failed");
    }

    let cluster = gtk::Box::new(gtk::Orientation::Vertical, 2);
    cluster.add_css_class("messages-bubble-cluster");
    cluster.set_hexpand(false);
    cluster.set_vexpand(false);

    let main = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    main.set_hexpand(false);
    main.set_halign(gtk::Align::Start);
    if !b.from_me {
        let face = face_tile(&b.who, 28);
        face.add_css_class("sm");
        face.set_valign(gtk::Align::End);
        face.set_hexpand(false);
        face.set_vexpand(false);
        main.append(&face);
    }

    let bub = gtk::Box::new(gtk::Orientation::Vertical, 2);
    bub.add_css_class("messages-bubble");
    bub.set_hexpand(false);
    if b.from_me {
        bub.add_css_class("me");
    }
    if !b.from_me {
        let who = gtk::Label::builder()
            .label(&b.who)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .css_classes(["messages-bubble-who", &format!("sw-{}", model::face_swatch(&b.who))])
            .build();
        bub.append(&who);
    }
    if !b.body.is_empty() {
        let markup = crate::link_preview::linkify_markup(&b.body);
        let body = gtk::Label::builder()
            .use_markup(true)
            .label(&markup)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(36)
            .css_classes(["messages-bubble-body"])
            .build();
        body.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
        body.set_hexpand(false);
        body.set_selectable(true);
        body.connect_activate_link(|_, uri| {
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::UriLauncher::new(uri).launch(None::<&gtk::Window>, gtk::gio::Cancellable::NONE, |_| {});
                let _ = display;
            }
            glib::Propagation::Stop
        });
        bub.append(&body);
        let auto = b.from_me || model::is_saved_contact(&b.who);
        schedule_link_previews(&bub, &b.body, &b.id, auto);
    }
    for att in &b.attachments {
        if att.mime.starts_with("image/") {
            bub.append(&image_attachment_widget(att));
        } else {
            let name = if att.name.is_empty() { "Attachment" } else { att.name.as_str() };
            let meta = if att.size > 0 {
                format_size_ui(att.size)
            } else if att.mime.is_empty() {
                "File".into()
            } else {
                att.mime.clone()
            };
            bub.append(&file_chip(name, &meta, Some(att)));
        }
    }
    if b.attachments.is_empty()
        && let Some((name, meta)) = &b.file
    {
        bub.append(&file_chip(name, meta, None));
    }
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    foot.add_css_class("messages-bubble-foot");
    foot.set_halign(gtk::Align::End);
    foot.set_valign(gtk::Align::Center);
    foot.append(&gtk::Label::new(Some(&b.when)));
    if b.from_me {
        foot.append(&status_check(&b.status));
    }
    if b.from_me && b.status == "failed" && !b.id.is_empty() {
        let retry = widgets::icon_button("view-refresh-symbolic", "Retry sending");
        retry.add_css_class("messages-failed-action");
        let id = b.id.clone();
        retry.connect_clicked(move |_| retry_failed(&id));
        foot.append(&retry);
        let delete = widgets::icon_button("user-trash-symbolic", "Delete message");
        delete.add_css_class("messages-failed-action");
        let id = b.id.clone();
        delete.connect_clicked(move |_| delete_failed(&id));
        foot.append(&delete);
    }
    bub.append(&foot);
    main.append(&bub);
    cluster.append(&main);
    if !b.reactions.is_empty() {
        let chips = reaction_chips(&b.reactions);
        if b.from_me {
            chips.set_halign(gtk::Align::End);
        }
        cluster.append(&chips);
    }
    if !b.id.is_empty() {
        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        let msg_id = b.id.clone();
        let bub_w = bub.clone();
        gesture.connect_released(move |_, _, x, y| {
            show_reaction_popover(&bub_w, &msg_id, x, y);
        });
        bub.add_controller(gesture);
        let long = gtk::GestureLongPress::new();
        let msg_id2 = b.id.clone();
        let bub_w2 = bub.clone();
        long.connect_pressed(move |_, x, y| {
            show_reaction_popover(&bub_w2, &msg_id2, x, y);
        });
        bub.add_controller(long);
    }

    // Width via Clamp; side via spacer (set_end alone fails when Clamp's
    // allocated width equals the bubble width). Height measure uses max when
    // for_size < 0 so this spacer layout does not collapse rows.
    let clamp = nyx_ui::clamp::Clamp::new(&cluster, max_cluster);
    clamp.add_css_class("messages-bubble-clamp");
    clamp.set_hexpand(false);
    clamp.set_start(true);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    spacer.set_vexpand(false);
    if b.from_me {
        row.append(&spacer);
        row.append(&clamp);
    } else {
        row.append(&clamp);
        row.append(&spacer);
    }
    set_bubble_max_width(row.upcast_ref(), max_cluster, chars);
    row
}

fn format_size_ui(n: i64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

fn status_check(status: &str) -> gtk::Image {
    let (icon, class) = match status {
        "failed" => ("dialog-error-symbolic", "failed"),
        "sending" => ("emblem-synchronizing-symbolic", "sending"),
        "read" => ("messages-check2-symbolic", "read"),
        "delivered" => ("messages-check2-symbolic", "delivered"),
        _ => ("messages-check-symbolic", "sent"),
    };
    let check = gtk::Image::from_icon_name(icon);
    check.set_pixel_size(14);
    check.set_valign(gtk::Align::Center);
    check.add_css_class("messages-status");
    check.add_css_class(class);
    check
}

fn reaction_chips(reactions: &[model::Reaction]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.add_css_class("messages-reactions");
    row.set_halign(gtk::Align::Start);
    for r in reactions {
        let label = if r.count > 1 { format!("{} {}", r.emoji, r.count) } else { r.emoji.clone() };
        let chip = gtk::Label::builder().label(&label).css_classes(["messages-reaction"]).build();
        if r.from_me {
            chip.add_css_class("mine");
        }
        row.append(&chip);
    }
    row
}

fn show_reaction_popover(parent: &impl IsA<gtk::Widget>, msg_id: &str, _x: f64, _y: f64) {
    let pop = gtk::Popover::new();
    pop.set_parent(parent);
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    box_.set_margin_top(6);
    box_.set_margin_bottom(6);
    box_.set_margin_start(6);
    box_.set_margin_end(6);
    const EMOJIS: &[&str] = &["👍", "❤️", "😂", "😮", "😢", "🙏"];
    let chat_id = current_ui().and_then(|u| u.selected.borrow().clone()).unwrap_or_default();
    for emoji in EMOJIS {
        let btn = gtk::Button::with_label(emoji);
        btn.add_css_class("flat");
        let mid = msg_id.to_string();
        let cid = chat_id.clone();
        let em = (*emoji).to_string();
        let pop2 = pop.clone();
        btn.connect_clicked(move |_| {
            let cid = cid.clone();
            let mid = mid.clone();
            let em = em.clone();
            crate::bg::run(
                move || service::messages_react(&cid, &mid, &em, false).map_err(|e| e.to_string()),
                |r| {
                    if let Err(e) = r {
                        eprintln!("nyx-messages: react: {e}");
                    }
                },
            );
            pop2.popdown();
        });
        box_.append(&btn);
    }
    pop.set_child(Some(&box_));
    pop.popup();
}

/// Add a link card to a bubble above its time/status footer (the footer is last).
fn insert_card(bub: &gtk::Box, card: &gtk::Box) {
    let foot = bub.last_child().filter(|w| w.has_css_class("messages-bubble-foot"));
    match foot {
        Some(foot) => bub.insert_child_after(card, foot.prev_sibling().as_ref()),
        None => bub.append(card),
    }
    // A plain queue_resize from the bubble doesn't reach this row's height through
    // NyxClamp (verified: the cards stayed unallocated). Invalidate every ancestor.
    let mut w = Some(bub.clone().upcast::<gtk::Widget>());
    while let Some(cur) = w {
        cur.queue_resize();
        w = cur.parent();
    }
}

/// Link cards for every URL in `body`. With `auto` off (unknown sender) nothing is
/// fetched until the user asks: each link gets a host-only card with a load button.
fn schedule_link_previews(bub: &gtk::Box, body: &str, msg_id: &str, auto: bool) {
    for url in crate::link_preview::extract_urls(body) {
        match crate::link_preview::cached(&url) {
            Some(Some(preview)) => add_link_card(bub, &url, &preview),
            Some(None) => {} // known failure
            None if auto => start_preview_fetch(bub, &url, msg_id),
            None => insert_card(bub, &link_stub_card(bub, &url, msg_id)),
        }
    }
}

fn start_preview_fetch(bub: &gtk::Box, url: &str, msg_id: &str) {
    if !crate::link_preview::begin_fetch(url) {
        return; // already fetching; the in-flight fetch finds this row by id
    }
    let url = url.to_string();
    let url2 = url.clone();
    let bub2 = bub.clone();
    let msg_id = msg_id.to_string();
    crate::bg::run(
        move || crate::link_preview::fetch(&url2),
        move |preview| {
            crate::link_preview::end_fetch(&url);
            let Some(preview) = preview else {
                crate::link_preview::store(&url, None);
                return;
            };
            crate::link_preview::store(&url, Some(preview.clone()));
            crate::link_preview::store(&preview.url, Some(preview.clone()));
            // The row may have been rebuilt (status change) while we fetched.
            let target =
                if bub2.parent().is_some() { Some(bub2.clone()) } else { current_ui().and_then(|u| bubble_box_for(&u, &msg_id)) };
            if let Some(bub) = target {
                add_link_card(&bub, &url, &preview);
            }
        },
    );
}

/// Host-only card for a link from an unknown sender, with a load-preview button.
fn link_stub_card(bub: &gtk::Box, url: &str, msg_id: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    card.add_css_class("messages-link-preview");
    card.add_css_class("messages-link-stub");
    card.set_widget_name(&format!("linkstub:{url}"));
    let host = crate::link_preview::host_of(url).unwrap_or_else(|| "link".into());
    card.append(
        &gtk::Label::builder()
            .label(&host)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["messages-link-host"])
            .build(),
    );
    let load = widgets::icon_button("image-loading-symbolic", "Load preview");
    load.set_valign(gtk::Align::Center);
    let (bub, url, msg_id) = (bub.clone(), url.to_string(), msg_id.to_string());
    load.connect_clicked(move |btn| {
        btn.set_sensitive(false);
        start_preview_fetch(&bub, &url, &msg_id);
    });
    card.append(&load);
    card
}

/// The `.messages-bubble` box inside the thread row for message `id`.
fn bubble_box_for(ui: &Ui, id: &str) -> Option<gtk::Box> {
    if id.is_empty() {
        return None;
    }
    let name = format!("bubble:{id}");
    let mut row = ui.thread.first_child();
    while let Some(w) = row {
        row = w.next_sibling();
        if w.widget_name() != name.as_str() {
            continue;
        }
        let mut stack = vec![w];
        while let Some(w) = stack.pop() {
            if w.has_css_class("messages-bubble") {
                return w.downcast::<gtk::Box>().ok();
            }
            let mut c = w.first_child();
            while let Some(x) = c {
                c = x.next_sibling();
                stack.push(x);
            }
        }
    }
    None
}

/// Add one link card per URL to a bubble (no duplicates when fetches race).
fn add_link_card(bub: &gtk::Box, url: &str, preview: &crate::link_preview::Preview) {
    let name = format!("link:{url}");
    let stub = format!("linkstub:{url}");
    let mut c = bub.first_child();
    while let Some(w) = c {
        c = w.next_sibling();
        if w.widget_name() == name.as_str() {
            return;
        }
        if w.widget_name() == stub.as_str() {
            bub.remove(&w);
        }
    }
    let card = link_preview_card(preview);
    card.set_widget_name(&name);
    if let Some(u) = current_ui() {
        set_bubble_max_width(card.upcast_ref(), u.bubble_max.get(), u.bubble_chars.get());
    }
    insert_card(bub, &card);
}

fn link_preview_card(p: &crate::link_preview::Preview) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("messages-link-preview");
    card.set_hexpand(false);
    if let Some(path) = &p.image_path {
        let pic = gtk::Picture::for_filename(path);
        pic.set_can_shrink(true);
        pic.set_content_fit(gtk::ContentFit::Cover);
        // Width is refreshed by set_bubble_max_width; start narrow so we don't
        // stretch the cluster to the image's intrinsic size.
        pic.set_size_request(280, 96);
        pic.set_hexpand(false);
        pic.add_css_class("messages-link-img");
        card.append(&pic);
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.add_css_class("messages-link-body");
    body.append(&gtk::Label::builder().label(&p.host).xalign(0.0).css_classes(["messages-link-host"]).build());
    let title = gtk::Label::builder()
        .label(&p.title)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(36)
        .css_classes(["messages-link-title"])
        .build();
    title.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
    body.append(&title);
    if !p.description.is_empty() {
        let desc = gtk::Label::builder()
            .label(&p.description)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(36)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .lines(2)
            .css_classes(["messages-link-desc"])
            .build();
        desc.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
        body.append(&desc);
    }
    card.append(&body);
    let url = p.url.clone();
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        gtk::UriLauncher::new(&url).launch(None::<&gtk::Window>, gtk::gio::Cancellable::NONE, |_| {});
    });
    card.add_controller(click);
    card
}

fn image_attachment_widget(att: &model::Attachment) -> gtk::Box {
    let wrap = gtk::Box::new(gtk::Orientation::Vertical, 4);
    wrap.add_css_class("messages-image-att");
    let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    placeholder.add_css_class("messages-image-ph");
    placeholder.set_size_request(180, 120);
    placeholder.add_css_class("messages-att-img");
    placeholder.set_halign(gtk::Align::Fill);
    let label = gtk::Label::new(Some("Photo"));
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    label.set_vexpand(true);
    placeholder.append(&label);
    wrap.append(&placeholder);
    let att_id = att.id.clone();
    let key = att.key_b64.clone();
    let name = att.name.clone();
    // Already downloaded this session: show it now, don't ask the daemon again.
    if let Some(path) = MEDIA_PATHS.with(|m| m.borrow().get(&att_id).cloned())
        && std::path::Path::new(&path).exists()
    {
        show_photo(&wrap, &path);
        return wrap;
    }
    let wrap2 = wrap.clone();
    if !att_id.is_empty() && !key.is_empty() {
        crate::bg::run(
            {
                let att_id = att_id.clone();
                move || service::media_download(&att_id, &key, &name).map_err(|e| e.to_string())
            },
            move |result| match result {
                Ok(path) => {
                    MEDIA_PATHS.with(|m| m.borrow_mut().insert(att_id, path.clone()));
                    show_photo(&wrap2, &path);
                }
                Err(e) => eprintln!("nyx-messages: media download: {e}"),
            },
        );
    }
    wrap
}

thread_local! {
    /// attachment id → downloaded file, so row rebuilds don't re-download photos.
    static MEDIA_PATHS: RefCell<std::collections::HashMap<String, String>> =
        RefCell::new(std::collections::HashMap::new());
}

fn show_photo(wrap: &gtk::Box, path: &str) {
    while let Some(c) = wrap.first_child() {
        wrap.remove(&c);
    }
    let pic = gtk::Picture::for_filename(path);
    pic.set_can_shrink(true);
    pic.set_content_fit(gtk::ContentFit::Contain);
    pic.set_size_request(180, -1);
    pic.add_css_class("messages-att-img");
    if let Some(u) = current_ui() {
        pic.set_size_request((u.bubble_max.get() - BUBBLE_PAD_X).clamp(1, 180), -1);
    }
    wrap.append(&pic);
    // Save: an icon over the photo's corner, shown while the pointer is on it.
    let save = widgets::icon_button("document-save-symbolic", "Save photo");
    save.add_css_class("messages-save");
    save.add_css_class("osd");
    save.set_halign(gtk::Align::End);
    save.set_valign(gtk::Align::Start);
    save.set_margin_top(6);
    save.set_margin_end(6);
    save.set_visible(false);
    let path2 = path.to_string();
    save.connect_clicked(move |_| save_media_as(&path2));
    wrap.remove(&pic);
    let over = gtk::Overlay::new();
    over.set_child(Some(&pic));
    over.add_overlay(&save);
    let hover = gtk::EventControllerMotion::new();
    hover.connect_enter({
        let save = save.clone();
        move |_, _, _| save.set_visible(true)
    });
    hover.connect_leave(move |_| save.set_visible(false));
    over.add_controller(hover);
    wrap.append(&over);
}

fn save_media_as(path: &str) {
    let Some(ui) = current_ui() else {
        return;
    };
    let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("attachment");
    let dialog = gtk::FileDialog::builder().title("Save attachment").modal(true).initial_name(name).build();
    let src = path.to_string();
    let win = ui.window.clone();
    dialog.save(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res
            && let Some(dest) = file.path()
        {
            let _ = std::fs::copy(&src, dest);
        }
    });
}

fn file_chip(name: &str, meta: &str, att: Option<&model::Attachment>) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    chip.add_css_class("messages-file");
    chip.set_valign(gtk::Align::Center);
    let ic = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    ic.add_css_class("messages-file-icon");
    ic.set_halign(gtk::Align::Center);
    ic.set_valign(gtk::Align::Center);
    ic.append(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
    chip.append(&ic);
    let tx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tx.set_hexpand(true);
    tx.set_valign(gtk::Align::Center);
    tx.append(&gtk::Label::builder().label(name).xalign(0.0).css_classes(["messages-file-name"]).build());
    tx.append(&gtk::Label::builder().label(meta).xalign(0.0).css_classes(["messages-file-meta"]).build());
    chip.append(&tx);
    if let Some(att) = att.filter(|a| !a.id.is_empty() && !a.key_b64.is_empty()) {
        let save = gtk::Button::from_icon_name("document-save-symbolic");
        save.add_css_class("flat");
        save.set_tooltip_text(Some("Save"));
        let id = att.id.clone();
        let key = att.key_b64.clone();
        let name = att.name.clone();
        save.connect_clicked(move |_| {
            crate::bg::run(
                {
                    let id = id.clone();
                    let key = key.clone();
                    let name = name.clone();
                    move || service::media_download(&id, &key, &name).map_err(|e| e.to_string())
                },
                move |result| {
                    if let Ok(path) = result {
                        save_media_as(&path);
                    }
                },
            );
        });
        chip.append(&save);
    }
    chip
}

fn face_tile(name: &str, size: i32) -> gtk::Label {
    let tile = gtk::Label::new(None);
    tile.set_size_request(size, size);
    tile.set_halign(gtk::Align::Center);
    tile.set_valign(gtk::Align::Center);
    tile.set_hexpand(false);
    tile.set_vexpand(false);
    tile.set_xalign(0.5);
    tile.set_yalign(0.5);
    tile.add_css_class("messages-face");
    if size <= 28 {
        tile.add_css_class("sm");
    }
    set_face(&tile, name);
    tile
}

fn set_face(tile: &gtk::Label, name: &str) {
    let sw = model::face_swatch(name);
    for i in 0..8 {
        tile.remove_css_class(&format!("sw-{i}"));
    }
    tile.add_css_class(&format!("sw-{sw}"));
    tile.set_label(&model::initials(name));
}

fn empty_state() -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.add_css_class("messages-empty");
    box_.set_halign(gtk::Align::Center);
    box_.set_valign(gtk::Align::Center);
    box_.set_vexpand(true);
    box_.set_hexpand(true);
    box_.append(&gtk::Image::from_icon_name("mail-unread-symbolic"));
    box_.append(&gtk::Label::builder().label("No conversation selected").css_classes(["messages-empty-title"]).build());
    box_.append(
        &gtk::Label::builder()
            .label("Pick a chat from the list, or start a new one.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["messages-empty-body"])
            .build(),
    );
    box_
}

fn arm_sync() {
    glib::idle_add_local_once(|| {
        if let Some(u) = current_ui() {
            sync_thread(&u);
        }
    });
}

/// Run `f` once the frame clock has laid out and painted the next frame.
fn after_next_layout(widget: &impl IsA<gtk::Widget>, f: fn()) {
    let Some(clock) = widget.frame_clock() else {
        glib::idle_add_local_once(f);
        return;
    };
    let id: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let id2 = id.clone();
    let handler = clock.connect_after_paint(move |c| {
        if let Some(h) = id2.borrow_mut().take() {
            c.disconnect(h);
        }
        f();
    });
    *id.borrow_mut() = Some(handler);
    clock.request_phase(gtk::gdk::FrameClockPhase::LAYOUT);
}

/// Line the header's person face up with the incoming-message faces below it.
fn align_header() {
    let Some(ui) = current_ui() else {
        return;
    };
    let hdr = &ui.hdr_person;
    if !hdr.is_visible() {
        if hdr.margin_start() != 0 {
            hdr.set_margin_start(0);
        }
        return;
    }
    let pad = if ui.window.has_css_class("narrow") { 10.0 } else { 18.0 };
    let Some(chat_col) = ui.root_stack.child_by_name("inbox") else {
        return;
    };
    if let (Some(pb), Some(cb)) = (hdr.compute_bounds(&ui.window), chat_col.compute_bounds(&ui.window)) {
        let delta = (cb.x() + pad - pb.x()).round() as i32;
        if delta.abs() > 1 {
            hdr.set_margin_start((hdr.margin_start() + delta).max(0));
        }
    }
}

/// Bubble width cap for a chat pane `w` px wide: 40% (full) or 70% (narrow).
fn bubble_widths(ui: &Ui, w: i32) -> (i32, i32) {
    let narrow = ui.window.has_css_class("narrow") || w < 520;
    let frac = if narrow { 0.70 } else { 0.40 };
    let max_cluster = ((w as f64) * frac).round() as i32;
    // ~7px per char at UI font; leave room for padding + avatar.
    let chars = ((max_cluster - 48) / 7).clamp(16, 40);
    (max_cluster, chars)
}

/// Keep short threads on the bottom (top pad) and bubbles at the pane's width cap.
fn sync_thread(ui: &Ui) {
    let w = ui.thread_scroll.width();
    let view_h = ui.thread_scroll.height();
    if w <= 0 || view_h <= 0 {
        return;
    }
    // The viewport sizes rows at their natural height, so the pad is view - natural.
    let (_, nat_h, _, _) = ui.thread.measure(gtk::Orientation::Vertical, w);
    let extra = (view_h - nat_h).max(0);
    if extra != ui.pad_h.get() {
        ui.pad_h.set(extra);
        ui.pad_css.load_from_string(&format!(".messages-thread-pad {{ min-height: {extra}px; }}"));
        ui.thread_pad.queue_resize();
    }
    let (max_cluster, chars) = bubble_widths(ui, w);
    if max_cluster == ui.bubble_max.get() && chars == ui.bubble_chars.get() {
        return;
    }
    ui.bubble_max.set(max_cluster);
    ui.bubble_chars.set(chars);
    let mut row = ui.thread.first_child();
    while let Some(widget) = row {
        row = widget.next_sibling();
        set_bubble_max_width(&widget, max_cluster, chars);
    }
}

/// Tell the model which chat is on screen (selected, window active): it gets no unread bumps.
fn update_open_chat(ui: &Ui) {
    let selected = ui.selected.borrow().clone();
    let open = selected.filter(|_| ui.window.is_active());
    model::set_open_chat(open.as_deref());
}

/// Mark the newest inbound message read on the phone. The badge is already cleared;
/// put `prev_unread` back if the daemon refuses.
fn mark_latest_read(chat_id: &str, prev_unread: u32) {
    if std::env::var_os("NYX_MESSAGES_MOCK").is_some() {
        return;
    }
    let Some(msg_id) = model::latest_inbound_id(chat_id) else {
        return;
    };
    let chat = chat_id.to_string();
    let chat2 = chat.clone();
    crate::bg::run(
        move || service::mark_read(&chat, &msg_id).map_err(|e| e.to_string()),
        move |r| {
            if let Err(e) = r {
                eprintln!("nyx-messages: mark_read: {e}");
                if prev_unread > 0 {
                    model::set_unread(&chat2, prev_unread);
                    if let Some(u) = current_ui() {
                        sync_list(&u);
                    }
                }
            }
        },
    );
}
