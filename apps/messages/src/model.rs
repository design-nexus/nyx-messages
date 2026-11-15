//! Conversation models — live data from nyx-messagesd (mock only if NYX_MESSAGES_MOCK=1).

use gtk::glib;
use nyx_messages_proto::{Attachment as ProtoAtt, Chat as ProtoChat, Message as ProtoMsg, Reaction as ProtoReaction};
use std::cell::RefCell;

#[derive(Clone, Debug)]
pub struct Chat {
    pub id: &'static str,
    pub name: &'static str,
    pub subtitle: &'static str,
    pub preview: &'static str,
    pub when: &'static str,
    pub unread: u32,
    pub starred: bool,
    pub attachment: bool,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub key_b64: String,
}

#[derive(Clone, Debug)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub from_me: bool,
}

#[derive(Clone, Debug)]
pub struct Bubble {
    pub id: String,
    pub who: String,
    pub body: String,
    pub when: String,
    /// Unix ms; used for day separators.
    pub timestamp_ms: i64,
    pub from_me: bool,
    /// sending | sent | delivered | read | failed | "" (inbound)
    pub status: String,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    /// Legacy single-file chip for mock / optimistic local paths.
    pub file: Option<(String, String)>,
    /// Optimistic sends: the local file to resend on retry.
    pub local_path: Option<String>,
}

thread_local! {
    static LIVE: RefCell<Live> = RefCell::new(Live {
        paired: false,
        chats: Vec::new(),
        threads: std::collections::HashMap::new(),
        mock_cleared_unread: std::collections::HashSet::new(),
        open_chat: None,
    });
}

struct Live {
    paired: bool,
    chats: Vec<OwnedChat>,
    /// chat_id → bubbles
    threads: std::collections::HashMap<String, Vec<Bubble>>,
    /// Mock-only: chats whose unread badge was cleared by opening them.
    mock_cleared_unread: std::collections::HashSet<String>,
    /// The chat the user is looking at (selected, window active): no unread bumps.
    open_chat: Option<String>,
}

pub fn set_open_chat(chat_id: Option<&str>) {
    LIVE.with(|l| l.borrow_mut().open_chat = chat_id.map(str::to_string));
}

fn is_local_pending(b: &Bubble) -> bool {
    b.from_me && is_local_id(&b.id) && (b.status == "sending" || b.status == "failed")
}

/// A sender shown by name (saved in the phone's contacts), not as a bare number.
/// Links from these senders get automatic previews; from strangers they don't,
/// since fetching a link tells its owner your IP address and when you read it.
pub fn is_saved_contact(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() || n == "Contact" {
        return false;
    }
    let digits = n.chars().filter(|c| c.is_ascii_digit()).count();
    digits * 2 < n.chars().count()
}

pub fn is_local_id(id: &str) -> bool {
    id.starts_with("local-")
}

/// A fresh id for an optimistic bubble (unique even within one millisecond).
pub fn new_local_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("local-{ms}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone, Debug)]
pub struct OwnedChat {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub preview: String,
    pub when: String,
    pub timestamp_ms: i64,
    pub unread: u32,
    pub starred: bool,
    pub attachment: bool,
}

fn use_mock() -> bool {
    std::env::var_os("NYX_MESSAGES_MOCK").is_some()
}

pub fn set_paired(paired: bool) {
    LIVE.with(|l| l.borrow_mut().paired = paired);
}

fn sort_chats_newest_first(chats: &mut [OwnedChat]) {
    chats.sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms).then_with(|| a.id.cmp(&b.id)));
}

fn sort_bubbles_oldest_first(bubbles: &mut [Bubble]) {
    bubbles.sort_by(|a, b| a.timestamp_ms.cmp(&b.timestamp_ms).then_with(|| a.id.cmp(&b.id)));
}

pub fn upsert_chat(c: &ProtoChat) {
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        let mut owned = owned_from_proto(c);
        if let Some(slot) = live.chats.iter_mut().find(|x| x.id == owned.id) {
            // Stale chat.upsert must not rewind a newer message-driven preview.
            if slot.timestamp_ms > owned.timestamp_ms {
                owned.timestamp_ms = slot.timestamp_ms;
                owned.when = slot.when.clone();
                if !slot.preview.is_empty() {
                    owned.preview = slot.preview.clone();
                }
            }
            *slot = owned;
        } else {
            live.chats.push(owned);
        }
        sort_chats_newest_first(&mut live.chats);
    });
}

pub fn replace_chats(chats: &[ProtoChat]) {
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        let prev: std::collections::HashMap<String, (i64, String, String)> =
            live.chats.iter().map(|c| (c.id.clone(), (c.timestamp_ms, c.preview.clone(), c.when.clone()))).collect();
        live.chats.clear();
        for c in chats {
            let mut owned = owned_from_proto(c);
            if let Some((ts, preview, when)) = prev.get(&owned.id)
                && *ts > owned.timestamp_ms
            {
                owned.timestamp_ms = *ts;
                owned.preview = preview.clone();
                owned.when = when.clone();
            }
            live.chats.push(owned);
        }
        sort_chats_newest_first(&mut live.chats);
    });
}

fn owned_from_proto(c: &ProtoChat) -> OwnedChat {
    let name = display_name(&c.name, &c.id);
    OwnedChat {
        id: c.id.clone(),
        name: name.clone(),
        subtitle: chat_subtitle(c.is_group, &name),
        preview: preview_text(&c.snippet),
        when: format_when(c.timestamp_ms),
        timestamp_ms: c.timestamp_ms,
        unread: if c.unread { 1 } else { 0 },
        starred: c.pinned,
        attachment: false,
    }
}

fn display_name(name: &str, fallback: &str) -> String {
    let n = name.trim();
    if n.is_empty() || is_protocol_notice(n) { fallback.to_string() } else { n.to_string() }
}

fn chat_subtitle(is_group: bool, _name: &str) -> String {
    // No "SMS" / "RCS" chrome — Google already stuffed that into notices we strip.
    if is_group { "Group".into() } else { String::new() }
}

/// Sidebar previews must be one visual line — SMS snippets often contain `\n`.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn preview_text(s: &str) -> String {
    let line = one_line(s);
    if is_protocol_notice(&line) { String::new() } else { line }
}

/// Google Messages auto-labels, not real chat text.
pub fn is_protocol_notice(s: &str) -> bool {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() {
        return false;
    }
    t.starts_with("texting with ") || t.starts_with("rcs chat with ") || (t.starts_with("texting ") && t.contains("(sms/mms)"))
}

pub fn upsert_message(m: &ProtoMsg) {
    if is_protocol_notice(&m.text) {
        return;
    }
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        let bubble = bubble_from_proto(m);
        let mut replaced = false;
        {
            let list = live.threads.entry(m.chat_id.clone()).or_default();
            // 1. same server id, 2. same tmp id (the daemon's, adopted on send),
            // 3. an optimistic bubble still pending with the same text — never an
            //    older real message that happens to say the same thing.
            let slot = list
                .iter()
                .position(|b| !b.id.is_empty() && b.id == bubble.id)
                .or_else(|| (!m.tmp_id.is_empty()).then(|| list.iter().position(|b| b.id == m.tmp_id)).flatten())
                .or_else(|| m.from_me.then(|| list.iter().position(|b| is_local_pending(b) && b.body == m.text)).flatten());
            if let Some(i) = slot {
                list[i] = bubble.clone();
                replaced = true;
            }
            if !replaced {
                list.push(bubble);
            }
            sort_bubbles_oldest_first(list);
        }
        touch_chat_from_msg(&mut live, m);
        let open = live.open_chat.as_deref() == Some(m.chat_id.as_str());
        if !m.from_me
            && !replaced
            && !open
            && let Some(chat) = live.chats.iter_mut().find(|c| c.id == m.chat_id)
        {
            chat.unread = chat.unread.saturating_add(1);
        }
    });
}

fn touch_chat_from_msg(live: &mut Live, m: &ProtoMsg) {
    if let Some(chat) = live.chats.iter_mut().find(|c| c.id == m.chat_id) {
        // Only move chat forward when this message is newer than the current preview.
        if m.timestamp_ms >= chat.timestamp_ms {
            let preview = if !m.text.trim().is_empty() {
                preview_text(&m.text)
            } else if !m.attachments.is_empty() {
                let a = &m.attachments[0];
                if a.mime.starts_with("image/") {
                    "Photo".into()
                } else if a.name.is_empty() {
                    "Attachment".into()
                } else {
                    a.name.clone()
                }
            } else {
                preview_text(&m.text)
            };
            chat.preview = preview;
            chat.when = format_when(m.timestamp_ms);
            chat.timestamp_ms = m.timestamp_ms;
            chat.attachment = !m.attachments.is_empty();
        }
        sort_chats_newest_first(&mut live.chats);
    }
}

fn att_from_proto(a: &ProtoAtt) -> Attachment {
    Attachment { id: a.id.clone(), name: a.name.clone(), mime: a.mime.clone(), size: a.size, key_b64: a.key_b64.clone() }
}

fn reaction_from_proto(r: &ProtoReaction) -> Reaction {
    Reaction { emoji: r.emoji.clone(), count: if r.count == 0 { 1 } else { r.count }, from_me: r.from_me }
}

fn bubble_from_proto(m: &ProtoMsg) -> Bubble {
    let attachments: Vec<Attachment> = m.attachments.iter().map(att_from_proto).collect();
    let file = attachments.first().map(|a| {
        let meta = if a.size > 0 {
            format_size(a.size)
        } else if a.mime.is_empty() {
            "File".into()
        } else {
            a.mime.clone()
        };
        let name = if a.name.is_empty() { "Attachment".into() } else { a.name.clone() };
        (name, meta)
    });
    Bubble {
        id: if !m.id.is_empty() { m.id.clone() } else { m.tmp_id.clone() },
        who: if m.from_me {
            "You".into()
        } else if m.sender.is_empty() {
            "Contact".into()
        } else {
            m.sender.clone()
        },
        body: m.text.clone(),
        when: format_clock(m.timestamp_ms),
        timestamp_ms: m.timestamp_ms,
        from_me: m.from_me,
        status: m.status.clone(),
        attachments,
        reactions: m.reactions.iter().map(reaction_from_proto).collect(),
        file,
        local_path: None,
    }
}

fn format_size(n: i64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Optimistic outgoing bubble before the daemon ack lands.
pub fn push_outgoing_optimistic(
    chat_id: &str,
    tmp_id: &str,
    text: &str,
    file: Option<(String, String)>,
    local_path: Option<String>,
) {
    let now = glib::DateTime::now_local().ok().map(|dt| dt.to_unix() * 1000).unwrap_or(0);
    let has_file = file.is_some();
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        {
            let list = live.threads.entry(chat_id.to_string()).or_default();
            list.push(Bubble {
                id: tmp_id.to_string(),
                who: "You".into(),
                body: text.into(),
                when: format_clock(now),
                timestamp_ms: now,
                from_me: true,
                status: "sending".into(),
                attachments: Vec::new(),
                reactions: Vec::new(),
                file,
                local_path,
            });
            sort_bubbles_oldest_first(list);
        }
        if let Some(chat) = live.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.preview = if text.trim().is_empty() { "Attachment".into() } else { preview_text(text) };
            chat.when = format_when(now);
            chat.timestamp_ms = now;
            chat.attachment = has_file;
        }
        sort_chats_newest_first(&mut live.chats);
    });
}

/// Mark an optimistic bubble failed (it offers retry / delete).
pub fn mark_bubble_failed(chat_id: &str, tmp_id: &str) {
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        if let Some(b) = live.threads.get_mut(chat_id).and_then(|list| list.iter_mut().find(|b| b.id == tmp_id)) {
            b.status = "failed".into();
        }
    });
}

/// The daemon accepted a send: adopt its tmp id so its echo replaces this bubble.
pub fn adopt_tmp_id(chat_id: &str, local_id: &str, daemon_tmp: &str) {
    if daemon_tmp.is_empty() {
        return;
    }
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        if let Some(list) = live.threads.get_mut(chat_id) {
            // The echo may already have landed (and replaced the bubble by text).
            if list.iter().any(|b| b.id == daemon_tmp) {
                list.retain(|b| b.id != local_id);
            } else if let Some(b) = list.iter_mut().find(|b| b.id == local_id) {
                b.id = daemon_tmp.to_string();
            }
        }
    });
}

/// Take a bubble out of a thread (delete a failed send, or before a retry).
pub fn remove_bubble(chat_id: &str, id: &str) -> Option<Bubble> {
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        let list = live.threads.get_mut(chat_id)?;
        let i = list.iter().position(|b| b.id == id)?;
        Some(list.remove(i))
    })
}

/// Restore unread after a failed mark_read.
pub fn set_unread(chat_id: &str, unread: u32) {
    LIVE.with(|l| {
        if let Some(chat) = l.borrow_mut().chats.iter_mut().find(|c| c.id == chat_id) {
            chat.unread = unread;
        }
    });
}

pub fn replace_messages(chat_id: &str, messages: &[ProtoMsg]) {
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        let mut bubbles: Vec<Bubble> = messages.iter().filter(|m| !is_protocol_notice(&m.text)).map(bubble_from_proto).collect();
        // Keep sends the server hasn't echoed yet (two quick sends, or a failure).
        if let Some(old) = live.threads.get(chat_id) {
            for b in old.iter().filter(|b| is_local_pending(b) || b.status == "sending") {
                let echoed = bubbles
                    .iter()
                    .any(|n| n.id == b.id || (n.from_me && n.body == b.body && n.timestamp_ms >= b.timestamp_ms - 60_000));
                if !echoed {
                    bubbles.push(b.clone());
                }
            }
        }
        sort_bubbles_oldest_first(&mut bubbles);
        live.threads.insert(chat_id.to_string(), bubbles);
    });
}

pub fn clear_unread(chat_id: &str) {
    LIVE.with(|l| {
        let mut live = l.borrow_mut();
        if use_mock() {
            live.mock_cleared_unread.insert(chat_id.to_string());
        }
        if let Some(chat) = live.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.unread = 0;
        }
    });
}

/// Latest inbound message id for mark-read, if any.
pub fn latest_inbound_id(chat_id: &str) -> Option<String> {
    LIVE.with(|l| {
        l.borrow()
            .threads
            .get(chat_id)
            .and_then(|bubbles| bubbles.iter().rev().find(|b| !b.from_me && !b.id.is_empty()).map(|b| b.id.clone()))
    })
}

/// Find an existing 1:1 chat whose name/id digits match a phone number.
pub fn find_chat_by_phone(phone: &str) -> Option<OwnedChat> {
    let digits = normalize_digits(phone);
    if digits.len() < 7 {
        return None;
    }
    owned_chats().into_iter().find(|c| {
        if c.subtitle == "Group" {
            return false;
        }
        let name_d = normalize_digits(&c.name);
        let id_d = normalize_digits(&c.id);
        phone_digits_match(&digits, &name_d) || phone_digits_match(&digits, &id_d)
    })
}

fn normalize_digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn phone_digits_match(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    // Compare last 10 digits when both are long enough (country-code variants).
    let a10 = if a.len() >= 10 { &a[a.len() - 10..] } else { a };
    let b10 = if b.len() >= 10 { &b[b.len() - 10..] } else { b };
    a10 == b10
}

pub fn owned_chats() -> Vec<OwnedChat> {
    if use_mock() {
        return LIVE.with(|l| {
            let cleared = &l.borrow().mock_cleared_unread;
            MOCK_CHATS
                .iter()
                .map(|c| OwnedChat {
                    id: c.id.to_string(),
                    name: c.name.to_string(),
                    subtitle: c.subtitle.to_string(),
                    preview: c.preview.to_string(),
                    when: c.when.to_string(),
                    timestamp_ms: 0,
                    unread: if cleared.contains(c.id) { 0 } else { c.unread },
                    starred: c.starred,
                    attachment: c.attachment,
                })
                .collect()
        });
    }
    LIVE.with(|l| l.borrow().chats.clone())
}

pub fn filter_owned(filter: &str) -> Vec<OwnedChat> {
    owned_chats()
        .into_iter()
        .filter(|c| match filter {
            "unread" => c.unread > 0,
            "starred" => c.starred,
            _ => true,
        })
        .collect()
}

pub fn owned_by_id(id: &str) -> Option<OwnedChat> {
    owned_chats().into_iter().find(|c| c.id == id)
}

pub fn bubbles_for_owned(id: &str) -> Vec<Bubble> {
    if use_mock() {
        return mock_bubbles(id);
    }
    LIVE.with(|l| l.borrow().threads.get(id).cloned().unwrap_or_default())
}

pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter(|w| !w.is_empty())
        .take(2)
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub fn face_swatch(name: &str) -> u8 {
    let mut h: u32 = 2166136261;
    for b in name.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(16777619);
    }
    (h % 8) as u8
}

fn local_dt(ms: i64) -> Option<glib::DateTime> {
    if ms <= 0 {
        return None;
    }
    glib::DateTime::from_unix_local(ms / 1000).ok()
}

fn start_of_local_day(dt: &glib::DateTime) -> Option<glib::DateTime> {
    glib::DateTime::new(&dt.timezone(), dt.year(), dt.month(), dt.day_of_month(), 0, 0, 0.0).ok()
}

/// Sidebar timestamp: time today, "Yesterday", weekday, else short date.
fn format_when(ms: i64) -> String {
    let Some(dt) = local_dt(ms) else {
        return String::new();
    };
    let Ok(now) = glib::DateTime::now_local() else {
        return format_clock(ms);
    };
    let Some(msg_day) = start_of_local_day(&dt) else {
        return format_clock(ms);
    };
    let Some(today) = start_of_local_day(&now) else {
        return format_clock(ms);
    };
    let day_diff = today.difference(&msg_day).as_days();
    match day_diff {
        0 => dt.format("%H:%M").map(|s| s.to_string()).unwrap_or_default(),
        1 => "Yesterday".into(),
        2..=6 => dt.format("%a").map(|s| s.to_string()).unwrap_or_default(),
        _ if dt.year() == now.year() => {
            dt.format("%b %-d").or_else(|_| dt.format("%b %d")).map(|s| s.to_string()).unwrap_or_default()
        }
        _ => dt.format("%b %-d, %Y").or_else(|_| dt.format("%b %d, %Y")).map(|s| s.to_string()).unwrap_or_default(),
    }
}

/// Bubble footer clock (local).
fn format_clock(ms: i64) -> String {
    local_dt(ms).and_then(|dt| dt.format("%H:%M").ok()).map(|s| s.to_string()).unwrap_or_default()
}

/// Day separator label above a run of bubbles.
pub fn format_day(ms: i64) -> String {
    let Some(dt) = local_dt(ms) else {
        return String::new();
    };
    let Ok(now) = glib::DateTime::now_local() else {
        return dt.format("%A, %b %-d").or_else(|_| dt.format("%A, %b %d")).map(|s| s.to_string()).unwrap_or_default();
    };
    let Some(msg_day) = start_of_local_day(&dt) else {
        return String::new();
    };
    let Some(today) = start_of_local_day(&now) else {
        return String::new();
    };
    let day_diff = today.difference(&msg_day).as_days();
    match day_diff {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => dt.format("%A").map(|s| s.to_string()).unwrap_or_default(),
        _ if dt.year() == now.year() => {
            dt.format("%A, %b %-d").or_else(|_| dt.format("%A, %b %d")).map(|s| s.to_string()).unwrap_or_default()
        }
        _ => dt.format("%b %-d, %Y").or_else(|_| dt.format("%b %d, %Y")).map(|s| s.to_string()).unwrap_or_default(),
    }
}

/// Local calendar day key for grouping (yyyyMMdd).
pub fn day_key(ms: i64) -> i32 {
    local_dt(ms).map(|dt| dt.year() * 10_000 + dt.month() * 100 + dt.day_of_month()).unwrap_or(0)
}

// --- mock (NYX_MESSAGES_MOCK=1) ---

const MOCK_CHATS: &[Chat] = &[
    Chat {
        id: "ada",
        name: "Ada Okafor",
        subtitle: "",
        preview: "Perfect — see you at 7",
        when: "19:42",
        unread: 0,
        starred: true,
        attachment: false,
    },
    Chat {
        id: "harbor",
        name: "Harbor Climbing",
        subtitle: "Group",
        preview: "Mia: Route cards are up",
        when: "Tue",
        unread: 3,
        starred: false,
        attachment: false,
    },
];

fn mock_bubbles(id: &str) -> Vec<Bubble> {
    // Stable local evening timestamps so day chips and sort stay consistent in mock.
    let base = glib::DateTime::now_local()
        .ok()
        .and_then(|now| glib::DateTime::new(&now.timezone(), now.year(), now.month(), now.day_of_month(), 19, 40, 0.0).ok())
        .map(|dt| dt.to_unix() * 1000)
        .unwrap_or(1_700_000_000_000);
    match id {
        "ada" => vec![
            Bubble {
                id: "m1".into(),
                who: "Ada Okafor".into(),
                body: "Are we still on for dinner? https://example.com/menu".into(),
                when: "19:40".into(),
                timestamp_ms: base,
                from_me: false,
                status: String::new(),
                attachments: Vec::new(),
                reactions: Vec::new(),
                file: None,
                local_path: None,
            },
            Bubble {
                id: "m2".into(),
                who: "You".into(),
                body: "Yes — 7 works. Here's the place I was thinking: https://www.wikipedia.org and also https://github.com".into(),
                when: "19:41".into(),
                timestamp_ms: base + 60_000,
                from_me: true,
                status: "read".into(),
                attachments: Vec::new(),
                reactions: vec![Reaction {
                    emoji: "👍".into(),
                    count: 1,
                    from_me: false,
                }],
                file: None,
                local_path: None,
            },
            Bubble {
                id: "m3".into(),
                who: "Ada Okafor".into(),
                body: "Perfect — see you at 7. Also, for the long write-up: this is a multi-sentence paragraph that used to stretch the bubble all the way across the chat pane. It should wrap inside a narrow bubble at about forty percent of the pane instead of reading like a full-width document column.".into(),
                when: "19:42".into(),
                timestamp_ms: base + 120_000,
                from_me: false,
                status: String::new(),
                attachments: Vec::new(),
                reactions: Vec::new(),
                file: None,
                local_path: None,
            },
        ],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, tmp: &str, text: &str, from_me: bool, ts: i64) -> ProtoMsg {
        serde_json::from_value(serde_json::json!({
            "id": id, "chat_id": "c1", "text": text, "from_me": from_me,
            "sender": if from_me { "" } else { "Ada" }, "timestamp_ms": ts,
            "tmp_id": tmp, "status": if from_me { "sent" } else { "" },
        }))
        .unwrap()
    }

    fn reset() {
        LIVE.with(|l| {
            let mut live = l.borrow_mut();
            live.threads.clear();
            live.chats.clear();
            live.open_chat = None;
        });
        let chat: ProtoChat = serde_json::from_value(serde_json::json!({
            "id": "c1", "name": "Ada", "snippet": "", "timestamp_ms": 1, "unread": false,
            "is_group": false, "pinned": false, "type": "sms",
        }))
        .unwrap();
        replace_chats(&[chat]);
    }

    fn thread() -> Vec<Bubble> {
        LIVE.with(|l| l.borrow().threads.get("c1").cloned().unwrap_or_default())
    }

    #[test]
    fn echo_replaces_pending_not_older_same_text() {
        reset();
        upsert_message(&msg("m1", "", "ok", true, 1_000));
        let local = new_local_id();
        push_outgoing_optimistic("c1", &local, "ok", None, None);
        upsert_message(&msg("m2", "", "ok", true, 2_000));
        let t = thread();
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(t.iter().any(|b| b.id == "m1"));
        assert!(t.iter().any(|b| b.id == "m2"));
        assert!(!t.iter().any(|b| b.status == "sending"));
    }

    #[test]
    fn adopted_tmp_id_matches_echo() {
        reset();
        let local = new_local_id();
        push_outgoing_optimistic("c1", &local, "hi", None, None);
        adopt_tmp_id("c1", &local, "tmp-9");
        upsert_message(&msg("m5", "tmp-9", "hi", true, 5_000));
        let t = thread();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, "m5");
    }

    #[test]
    fn replace_keeps_unechoed_pending_sends() {
        reset();
        let a = new_local_id();
        let b = new_local_id();
        assert_ne!(a, b);
        push_outgoing_optimistic("c1", &a, "first", None, None);
        push_outgoing_optimistic("c1", &b, "second", None, None);
        let now = thread()[0].timestamp_ms;
        replace_messages("c1", &[msg("m1", "", "first", true, now)]);
        let t = thread();
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(t.iter().any(|x| x.body == "second" && x.status == "sending"));
    }

    #[test]
    fn saved_contact_vs_number() {
        assert!(is_saved_contact("Ada Okafor"));
        assert!(is_saved_contact("Mia (climbing)"));
        assert!(!is_saved_contact("+1 555 010 0199"));
        assert!(!is_saved_contact("5550100"));
        assert!(!is_saved_contact("Contact"));
        assert!(!is_saved_contact(""));
    }

    #[test]
    fn no_unread_for_open_chat() {
        reset();
        set_open_chat(Some("c1"));
        upsert_message(&msg("m1", "", "hey", false, 10));
        assert_eq!(owned_by_id("c1").unwrap().unread, 0);
        set_open_chat(None);
        upsert_message(&msg("m2", "", "hey again", false, 20));
        assert_eq!(owned_by_id("c1").unwrap().unread, 1);
    }
}
