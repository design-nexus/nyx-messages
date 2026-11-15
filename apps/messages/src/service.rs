//! Talks to nyx-messagesd: status, live sync, event fan-in on the GTK main loop.

use crate::model;
use nyx_messages_proto::{self as proto, Chat, Event, Message};

pub fn spawn_event_thread(tx: async_channel::Sender<Event>) {
    std::thread::spawn(move || {
        let _ = proto::event_stream(move |evt| {
            let _ = tx.send_blocking(evt);
        });
    });
}

pub fn refresh_status() -> Result<proto::Status, proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.status()
}

/// Fetch chats on a worker thread. Apply with [`model::replace_chats`] on the GTK thread only.
pub fn fetch_chats() -> Result<Vec<Chat>, proto::Error> {
    let mut last = proto::Error::Daemon("no attempt".into());
    for attempt in 0..20 {
        match proto::Client::connect_or_start() {
            Ok(mut c) => match c.chats_list() {
                Ok(chats) => {
                    // Empty right after connecting can mean "still syncing"; once the
                    // daemon says it's connected, empty means empty.
                    let connected = c.status().map(|s| s.connected).unwrap_or(false);
                    if chats.is_empty() && !connected && attempt < 19 {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    }
                    return Ok(chats);
                }
                Err(e) => {
                    let msg = e.to_string();
                    last = e;
                    if msg.contains("connecting") || msg.contains("not connected") {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    }
                    return Err(last);
                }
            },
            Err(e) => {
                last = e;
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }
    Err(last)
}

pub fn fetch_messages(chat_id: &str) -> Result<Vec<Message>, proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.messages_list(chat_id, 80)
}

pub fn send_text(chat_id: &str, text: &str) -> Result<String, proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.messages_send(chat_id, text)
}

pub fn send_media(chat_id: &str, text: &str, path: &str) -> Result<String, proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.messages_send_media(chat_id, text, Some(path))
}

pub fn get_or_create_chat(phone: &str) -> Result<Chat, proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.chats_get_or_create(phone)
}

pub fn mark_read(chat_id: &str, message_id: &str) -> Result<(), proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.messages_mark_read(chat_id, message_id)
}

pub fn media_download(media_id: &str, key_b64: &str, name: &str) -> Result<String, proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.media_download(media_id, key_b64, name)
}

pub fn typing_set(chat_id: &str, typing: bool) -> Result<(), proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.typing_set(chat_id, typing)
}

pub fn messages_react(chat_id: &str, message_id: &str, emoji: &str, remove: bool) -> Result<(), proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.messages_react(chat_id, message_id, emoji, remove)
}

pub fn logout() -> Result<(), proto::Error> {
    let mut c = proto::Client::connect_or_start()?;
    c.logout()?;
    Ok(())
}

pub fn apply_event(evt: &Event) {
    match evt {
        Event::ChatUpsert { chat } => model::upsert_chat(chat),
        Event::MessageUpsert { message } => model::upsert_message(message),
        Event::PairOk { .. } => model::set_paired(true),
        Event::Typing { .. }
        | Event::ChatsRefresh
        | Event::NeedsRepair { .. }
        | Event::Connection { .. }
        | Event::PairEmoji { .. }
        | Event::PairFail { .. } => {}
    }
}
