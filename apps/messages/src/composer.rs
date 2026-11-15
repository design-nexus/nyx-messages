//! Message composer: a TextView that grows to five lines, with a placeholder.
//! Enter sends; Shift+Enter (or Ctrl+Enter) starts a new line.

use gtk::glib;
use gtk::prelude::*;

/// Lines the field grows to before it scrolls.
const MAX_LINES: i32 = 5;

#[derive(Clone)]
pub struct Composer {
    /// Put this in the layout.
    pub root: gtk::Overlay,
    view: gtk::TextView,
    placeholder: gtk::Label,
}

impl Composer {
    pub fn new() -> Self {
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .hexpand(true)
            .css_classes(["messages-composer-entry"])
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .hexpand(true)
            .child(&view)
            .css_classes(["messages-composer-scroll"])
            .build();
        let placeholder = gtk::Label::builder()
            .xalign(0.0)
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Center)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .can_target(false)
            .css_classes(["messages-composer-placeholder"])
            .build();
        let root = gtk::Overlay::new();
        root.set_hexpand(true);
        root.set_valign(gtk::Align::Center);
        root.set_child(Some(&scroll));
        root.add_overlay(&placeholder);

        // Cap the height at MAX_LINES of the current font.
        view.connect_realize({
            let scroll = scroll.clone();
            move |v| {
                let layout = v.create_pango_layout(Some("Ag"));
                let (_, h) = layout.pixel_size();
                scroll.set_max_content_height(h.max(14) * MAX_LINES + 16);
            }
        });
        let c = Self { root, view, placeholder };
        c.view.buffer().connect_changed({
            let ph = c.placeholder.clone();
            move |b| ph.set_visible(b.char_count() == 0)
        });
        c
    }

    pub fn text(&self) -> String {
        let b = self.view.buffer();
        b.text(&b.start_iter(), &b.end_iter(), false).to_string()
    }

    pub fn set_text(&self, text: &str) {
        self.view.buffer().set_text(text);
    }

    pub fn set_placeholder_text(&self, text: Option<&str>) {
        self.placeholder.set_text(text.unwrap_or(""));
    }

    pub fn grab_focus(&self) {
        self.view.grab_focus();
    }

    /// Insert at the cursor (e.g. an emoji) and keep typing focus.
    pub fn insert_at_cursor(&self, text: &str) {
        self.view.buffer().insert_at_cursor(text);
        self.view.grab_focus();
    }

    pub fn connect_changed(&self, f: impl Fn(&Self) + 'static) {
        let me = self.clone();
        self.view.buffer().connect_changed(move |_| f(&me));
    }

    /// Enter (without Shift/Ctrl) — send.
    pub fn connect_activate(&self, f: impl Fn() + 'static) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let enter = matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter);
            let newline = mods.intersects(gtk::gdk::ModifierType::SHIFT_MASK | gtk::gdk::ModifierType::CONTROL_MASK);
            if enter && !newline {
                f();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.view.add_controller(keys);
    }
}
