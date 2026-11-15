//! Symbolic icons for Messages chrome (filled paths; GTK paints them).

pub fn install() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let dir = nyx_core::paths::cache_dir().join("messages-icons");
    let _ = std::fs::create_dir_all(&dir);
    for (name, body) in ICONS {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" viewBox=\"0 0 24 24\"><g fill=\"#000000\">{body}</g></svg>\n"
        );
        let path = dir.join(format!("{name}.svg"));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(svg.as_str()) {
            let _ = nyx_core::cmd::atomic_write(&path, &svg);
        }
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
}

const ICONS: &[(&str, &str)] = &[
    (
        "messages-chat-symbolic",
        r#"<path fill-rule="evenodd" d="M5.5 3h13A3.5 3.5 0 0 1 22 6.5v8a3.5 3.5 0 0 1-3.5 3.5H12l-5.2 3.7A.5.5 0 0 1 6 21.3V18h-.5A3.5 3.5 0 0 1 2 14.5v-8A3.5 3.5 0 0 1 5.5 3zm0 2A1.5 1.5 0 0 0 4 6.5v8A1.5 1.5 0 0 0 5.5 16H8v2.2l3.4-2.2h7.1a1.5 1.5 0 0 0 1.5-1.5v-8A1.5 1.5 0 0 0 18.5 5z"/>"#,
    ),
    (
        "messages-archive-symbolic",
        r#"<path fill-rule="evenodd" d="M3.2 3.5h17.6c.7 0 1.2.5 1.2 1.2v3.1c0 .7-.5 1.2-1.2 1.2H3.2C2.5 9 2 8.5 2 7.8V4.7c0-.7.5-1.2 1.2-1.2zM3.5 10.5h17V18a2.5 2.5 0 0 1-2.5 2.5H6A2.5 2.5 0 0 1 3.5 18zm6 2.5a1 1 0 0 0 0 2h5a1 1 0 0 0 0-2z"/>"#,
    ),
    ("messages-check-symbolic", r#"<path d="M4.2 12.4 9.6 17.6 19.8 6.2 18.2 4.8 9.6 14.6 5.8 11z"/>"#),
    (
        "messages-check2-symbolic",
        r#"<path d="M1.8 12.4 5.6 16.2 12.8 7.6 11.2 6.2 5.6 13.2 3.4 11z"/><path d="M9.4 16.2 16.8 7.4 15.2 6 9.4 13.2 7.6 11.4 6.2 12.8z"/>"#,
    ),
    (
        "messages-attach-symbolic",
        r#"<path fill-rule="evenodd" d="M16.5 6.5v9.2a4.5 4.5 0 0 1-9 0V6.8a2.8 2.8 0 0 1 5.6 0v8.2a1.1 1.1 0 1 1-2.2 0V7.6a.9.9 0 1 0-1.8 0v7.4a2.8 2.8 0 0 0 5.6 0V6.8a4.5 4.5 0 0 0-9 0v8.9a6.2 6.2 0 0 0 12.4 0V6.5a.9.9 0 1 0-1.8 0z"/>"#,
    ),
    (
        "messages-mic-symbolic",
        r#"<path d="M12 2a3.5 3.5 0 0 0-3.5 3.5v6a3.5 3.5 0 1 0 7 0v-6A3.5 3.5 0 0 0 12 2z"/><path d="M6.5 10.5a.9.9 0 0 0-1.8 0 7.3 7.3 0 0 0 6.4 7.25V20.5H9.2a.9.9 0 1 0 0 1.8h5.6a.9.9 0 1 0 0-1.8h-1.9v-2.75A7.3 7.3 0 0 0 19.3 10.5a.9.9 0 1 0-1.8 0 5.5 5.5 0 0 1-11 0z"/>"#,
    ),
    (
        "messages-send-symbolic",
        r#"<path d="M3.2 11.1 19.6 3.4a1 1 0 0 1 1.35 1.2L16.2 20.4a1 1 0 0 1-1.82.12l-2.7-5.9-5.9-2.7a1 1 0 0 1 .12-1.82z"/>"#,
    ),
    (
        "messages-call-symbolic",
        r#"<path d="M7.2 3.4c.6-.6 1.6-.6 2.1 0l1.6 1.6c.5.5.6 1.3.2 1.9L9.8 9.2c.8 1.5 2 2.7 3.5 3.5l2.3-1.3c.6-.4 1.4-.3 1.9.2l1.6 1.6c.6.5.6 1.5 0 2.1l-1.5 1.5c-.7.7-1.7 1-2.7.8C9.2 16.3 5.2 12.3 3.9 6.6c-.2-1 .1-2 .8-2.7z"/>"#,
    ),
    (
        "messages-phone-symbolic",
        r#"<path fill-rule="evenodd" d="M8.5 2.5h7A2.5 2.5 0 0 1 18 5v14a2.5 2.5 0 0 1-2.5 2.5h-7A2.5 2.5 0 0 1 6 19V5A2.5 2.5 0 0 1 8.5 2.5zM9 5h6v12H9zm2.2 13.2a1 1 0 1 0 1.6 0 1 1 0 0 0-1.6 0z"/>"#,
    ),
    (
        "messages-info-symbolic",
        r#"<path fill-rule="evenodd" d="M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm0 4.2a1.3 1.3 0 1 1 0 2.6 1.3 1.3 0 0 1 0-2.6zM10.7 11h1.1v5.2c0 .6.4 1 1 1h.5a.9.9 0 1 0 0-1.8h-.3V11c0-.6-.5-1.1-1.1-1.1h-1.2a.9.9 0 1 0 0 1.8z"/>"#,
    ),
    (
        "messages-smile-symbolic",
        r#"<path fill-rule="evenodd" d="M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm-3.2 7.2a1.2 1.2 0 1 1 0 2.4 1.2 1.2 0 0 1 0-2.4zm6.4 0a1.2 1.2 0 1 1 0 2.4 1.2 1.2 0 0 1 0-2.4zM8.2 14.2a.9.9 0 0 1 1.25.2A3.6 3.6 0 0 0 12 15.6c1 0 1.9-.4 2.55-1.2a.9.9 0 1 1 1.45 1.05A5.4 5.4 0 0 1 12 17.4a5.4 5.4 0 0 1-4-1.95.9.9 0 0 1 .2-1.25z"/>"#,
    ),
    (
        "messages-compose-symbolic",
        r#"<path d="M4 16.5 5.8 11.2 14.6 2.4a2 2 0 0 1 2.8 2.8L8.6 14.1z"/><path d="M3 19h7v1.6H3z"/>"#,
    ),
];
