//! Wire protocol for `nyx-messagesd` (newline-delimited JSON on a Unix socket).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const SERVICE: &str = "nyx-messagesd.service";

/// `$XDG_RUNTIME_DIR/nyx-messages/bus`
pub fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join("nyx-messages").join("bus"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub timestamp_ms: i64,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub is_group: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub r#type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub size: i64,
    /// Base64-encoded AES key for DownloadMedia.
    #[serde(default)]
    pub key_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub from_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub chat_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub from_me: bool,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub timestamp_ms: i64,
    #[serde(default)]
    pub tmp_id: String,
    /// sending | sent | delivered | read | failed | "" (inbound)
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub paired: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum Event {
    #[serde(rename = "pair.emoji")]
    PairEmoji {
        emoji: String,
        #[serde(default)]
        account: String,
    },
    #[serde(rename = "pair.ok")]
    PairOk {
        #[serde(default)]
        account: String,
    },
    #[serde(rename = "pair.fail")]
    PairFail {
        #[serde(default)]
        error: String,
    },
    #[serde(rename = "connection")]
    Connection {
        state: String,
        #[serde(default)]
        error: String,
    },
    #[serde(rename = "session.needs_repair")]
    NeedsRepair {
        #[serde(default)]
        error: String,
    },
    #[serde(rename = "chat.upsert")]
    ChatUpsert { chat: Chat },
    #[serde(rename = "message.upsert")]
    MessageUpsert { message: Message },
    #[serde(rename = "chats.refresh")]
    ChatsRefresh,
    #[serde(rename = "typing")]
    Typing {
        chat_id: String,
        #[serde(default)]
        typing: bool,
    },
}

#[derive(Debug)]
pub enum Error {
    NoRuntimeDir,
    Connect(std::io::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    Daemon(String),
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRuntimeDir => write!(f, "XDG_RUNTIME_DIR is not set"),
            Self::Connect(e) => write!(f, "connect: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Json(e) => write!(f, "json: {e}"),
            Self::Daemon(e) => write!(f, "{e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
        }
    }
}

impl std::error::Error for Error {}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

/// One-shot request/response over a fresh connection (events on other conns still work).
pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    pub fn connect() -> Result<Self, Error> {
        let path = socket_path().ok_or(Error::NoRuntimeDir)?;
        let stream = UnixStream::connect(&path).map_err(Error::Connect)?;
        // Gaia pair waits on the phone (daemon pairTimeout is 5 minutes).
        stream
            .set_read_timeout(Some(Duration::from_secs(360)))
            .map_err(Error::Io)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(Error::Io)?;
        let reader = BufReader::new(stream.try_clone().map_err(Error::Io)?);
        Ok(Self { stream, reader })
    }

    /// Connect, starting the daemon if needed.
    pub fn connect_or_start() -> Result<Self, Error> {
        match Self::connect() {
            Ok(c) => Ok(c),
            Err(_) => {
                if !start_daemon() {
                    return Err(Error::Daemon(
                        "couldn't start nyx-messagesd — is it installed?".into(),
                    ));
                }
                for _ in 0..50 {
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(c) = Self::connect() {
                        return Ok(c);
                    }
                }
                Err(Error::Daemon("nyx-messagesd did not become ready".into()))
            }
        }
    }

    fn call(&mut self, mut body: serde_json::Value) -> Result<serde_json::Value, Error> {
        let id = next_id();
        body.as_object_mut()
            .ok_or_else(|| Error::Protocol("request must be an object".into()))?
            .insert("id".into(), serde_json::Value::String(id.clone()));
        let mut line = serde_json::to_string(&body).map_err(Error::Json)?;
        line.push('\n');
        self.stream.write_all(line.as_bytes()).map_err(Error::Io)?;
        self.stream.flush().map_err(Error::Io)?;

        loop {
            let mut buf = String::new();
            self.reader.read_line(&mut buf).map_err(Error::Io)?;
            if buf.is_empty() {
                return Err(Error::Protocol("daemon closed connection".into()));
            }
            let v: serde_json::Value = serde_json::from_str(buf.trim_end()).map_err(Error::Json)?;
            // Skip async events on this connection.
            if v.get("event").is_some() {
                continue;
            }
            let resp_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if resp_id != id {
                continue;
            }
            let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
            if !ok {
                let err = v
                    .get("error")
                    .and_then(|x| x.as_str())
                    .unwrap_or("request failed")
                    .to_string();
                return Err(Error::Daemon(err));
            }
            return Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
        }
    }

    pub fn status(&mut self) -> Result<Status, Error> {
        let v = self.call(serde_json::json!({ "cmd": "status" }))?;
        serde_json::from_value(v).map_err(Error::Json)
    }

    pub fn pair_start(
        &mut self,
        cookies: HashMap<String, String>,
        force_new: bool,
    ) -> Result<serde_json::Value, Error> {
        self.call(serde_json::json!({
            "cmd": "pair.start",
            "cookies": cookies,
            "force_new": force_new,
        }))
    }

    pub fn pair_cancel(&mut self) -> Result<(), Error> {
        self.call(serde_json::json!({ "cmd": "pair.cancel" }))?;
        Ok(())
    }

    pub fn logout(&mut self) -> Result<(), Error> {
        self.call(serde_json::json!({ "cmd": "logout" }))?;
        Ok(())
    }

    pub fn chats_list(&mut self) -> Result<Vec<Chat>, Error> {
        let v = self.call(serde_json::json!({ "cmd": "chats.list" }))?;
        let chats = v
            .get("chats")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        serde_json::from_value(chats).map_err(Error::Json)
    }

    pub fn chats_get_or_create(&mut self, phone: &str) -> Result<Chat, Error> {
        let v = self.call(serde_json::json!({
            "cmd": "chats.get_or_create",
            "phone": phone,
        }))?;
        let chat = v
            .get("chat")
            .cloned()
            .ok_or_else(|| Error::Protocol("missing chat in get_or_create".into()))?;
        serde_json::from_value(chat).map_err(Error::Json)
    }

    pub fn messages_list(&mut self, chat_id: &str, count: i32) -> Result<Vec<Message>, Error> {
        let v = self.call(serde_json::json!({
            "cmd": "messages.list",
            "chat_id": chat_id,
            "count": count,
        }))?;
        let messages = v
            .get("messages")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        serde_json::from_value(messages).map_err(Error::Json)
    }

    pub fn messages_send(&mut self, chat_id: &str, text: &str) -> Result<String, Error> {
        self.messages_send_media(chat_id, text, None)
    }

    pub fn messages_send_media(
        &mut self,
        chat_id: &str,
        text: &str,
        path: Option<&str>,
    ) -> Result<String, Error> {
        let mut body = serde_json::json!({
            "cmd": "messages.send",
            "chat_id": chat_id,
            "text": text,
        });
        if let Some(p) = path {
            body["path"] = serde_json::Value::String(p.to_string());
        }
        let v = self.call(body)?;
        Ok(v
            .get("tmp_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn messages_mark_read(&mut self, chat_id: &str, message_id: &str) -> Result<(), Error> {
        self.call(serde_json::json!({
            "cmd": "messages.mark_read",
            "chat_id": chat_id,
            "message_id": message_id,
        }))?;
        Ok(())
    }

    pub fn media_download(
        &mut self,
        media_id: &str,
        key_b64: &str,
        name: &str,
    ) -> Result<String, Error> {
        let v = self.call(serde_json::json!({
            "cmd": "media.download",
            "media_id": media_id,
            "key_b64": key_b64,
            "name": name,
        }))?;
        Ok(v
            .get("path")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn typing_set(&mut self, chat_id: &str, typing: bool) -> Result<(), Error> {
        self.call(serde_json::json!({
            "cmd": "typing.set",
            "chat_id": chat_id,
            "typing": typing,
        }))?;
        Ok(())
    }

    pub fn messages_react(
        &mut self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
        remove: bool,
    ) -> Result<(), Error> {
        self.call(serde_json::json!({
            "cmd": "messages.react",
            "chat_id": chat_id,
            "message_id": message_id,
            "emoji": emoji,
            "remove": remove,
        }))?;
        Ok(())
    }
}

/// Listen for daemon push events on a dedicated connection.
pub fn event_stream(mut on_event: impl FnMut(Event) + Send + 'static) -> Result<(), Error> {
    let mut client = Client::connect_or_start()?;
    // Keep the connection open; only read events (ignore stray responses).
    loop {
        let mut buf = String::new();
        client.reader.read_line(&mut buf).map_err(Error::Io)?;
        if buf.is_empty() {
            return Err(Error::Protocol("event stream closed".into()));
        }
        let v: serde_json::Value = serde_json::from_str(buf.trim_end()).map_err(Error::Json)?;
        if v.get("event").is_none() {
            continue;
        }
        match serde_json::from_value::<Event>(v) {
            Ok(evt) => on_event(evt),
            Err(_) => continue,
        }
    }
}

fn start_daemon() -> bool {
    if std::path::Path::new(&dirs_config_systemd()).exists() {
        if std::process::Command::new("systemctl")
            .args(["--user", "start", SERVICE])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return true;
        }
    }
    let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("nyx-messagesd")))
    else {
        return false;
    };
    sibling.exists()
        && std::process::Command::new(sibling)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
}

fn dirs_config_systemd() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    config.join("systemd/user").join(SERVICE)
}
