//! Telegram Bot API client (blocking). The bot token is part of every URL, so
//! it is scrubbed from every error text and never printed or logged.

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

const API_BASE: &str = "https://api.telegram.org";
/// Update kinds the bot asks for.
pub const ALLOWED_UPDATES: [&str; 2] = ["message", "callback_query"];
const SHORT_TIMEOUT: Duration = Duration::from_secs(20);
/// Uploads take longer than a JSON call.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Telegram's caption limit (UTF-16 units).
pub const CAPTION_LIMIT: usize = 1024;
/// Largest photo / document the Bot API takes.
pub const MAX_PHOTO_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 50 * 1024 * 1024;
/// Largest file the Bot API lets a bot download (`getFile`).
pub const MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;

/// How a file goes out: `sendPhoto` (inline image) or `sendDocument` (any file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Photo,
    Document,
}

impl FileKind {
    pub fn method(self) -> &'static str {
        match self {
            FileKind::Photo => "sendPhoto",
            FileKind::Document => "sendDocument",
        }
    }
    pub fn field(self) -> &'static str {
        match self {
            FileKind::Photo => "photo",
            FileKind::Document => "document",
        }
    }
    pub fn max_bytes(self) -> u64 {
        match self {
            FileKind::Photo => MAX_PHOTO_BYTES,
            FileKind::Document => MAX_DOCUMENT_BYTES,
        }
    }
}

/// Content-Type from the file extension; unknown ones are `application/octet-stream`.
pub fn guess_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Builds a multipart/form-data body by hand: text `fields`, then one file part.
/// (reqwest's `multipart` feature is not enabled and may not be added.)
pub fn multipart_body(
    boundary: &str,
    fields: &[(&str, &str)],
    file_field: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
) -> Vec<u8> {
    let clean = |s: &str| s.replace(['"', '\r', '\n', '\\'], "_");
    let mut out = Vec::with_capacity(data.len() + 512);
    for (name, value) in fields {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{value}\r\n",
                clean(name)
            )
            .as_bytes(),
        );
    }
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {content_type}\r\n\r\n",
            clean(file_field),
            clean(filename)
        )
        .as_bytes(),
    );
    out.extend_from_slice(data);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

/// A boundary that does not occur in `data`.
fn pick_boundary(data: &[u8]) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    for n in 0u32..32 {
        let b = format!("----colony{:x}{:x}{n}", nanos, std::process::id());
        let needle = b.as_bytes();
        if !data.windows(needle.len()).any(|w| w == needle) {
            return b;
        }
    }
    "----colony-boundary-fallback-7f3a9c".to_string()
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BotInfo {
    pub id: i64,
    pub username: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
}

/// A tap on an inline button.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    /// The bot's message the button sits on.
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub data: Option<String>,
}

/// The message a swipe-reply points at (only its id is used).
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReplyRef {
    pub message_id: i64,
}

/// A file attached to a message (photo size, document, video, audio or voice note).
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct FileRef {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Answer of `getFile`.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct FileInfo {
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// Answer of `sendMessage`: only the new message's id.
#[derive(Deserialize, Debug)]
struct Sent {
    message_id: i64,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<User>,
    pub chat: Chat,
    /// Unix time the message was sent. Missing counts as 0 (oldest).
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub text: Option<String>,
    /// Set when the user swipe-replied to another message.
    #[serde(default)]
    pub reply_to_message: Option<ReplyRef>,
    /// Caption of a photo / document / video message.
    #[serde(default)]
    pub caption: Option<String>,
    /// Photo sizes, smallest first.
    #[serde(default)]
    pub photo: Vec<FileRef>,
    #[serde(default)]
    pub document: Option<FileRef>,
    #[serde(default)]
    pub video: Option<FileRef>,
    #[serde(default)]
    pub audio: Option<FileRef>,
    #[serde(default)]
    pub voice: Option<FileRef>,
}

impl Message {
    /// The attached file to download: the largest photo, else document, video, audio or voice.
    /// The flag tells a photo (default extension jpg).
    pub fn attachment(&self) -> Option<(&FileRef, bool)> {
        if let Some(p) = self.photo.iter().max_by_key(|p| p.file_size.unwrap_or(0)).or(self.photo.last()) {
            return Some((p, true));
        }
        [&self.document, &self.video, &self.audio, &self.voice]
            .into_iter()
            .find_map(|f| f.as_ref())
            .map(|f| (f, false))
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct User {
    pub id: i64,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
}

pub trait TelegramApi {
    fn get_me(&self) -> Result<BotInfo>;
    fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>>;
    /// Returns the `message_id` of the sent message.
    fn send_message(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> Result<i64>;
    /// Sends `html` with `parse_mode` HTML. Default: plain send (for wrappers).
    fn send_html(&self, chat_id: i64, html: &str, reply_to: Option<i64>) -> Result<i64> {
        self.send_message(chat_id, html, reply_to)
    }
    /// Sends plain `text` with an inline keyboard (`reply_markup`). Default: no keyboard (for wrappers).
    fn send_keyboard(&self, chat_id: i64, text: &str, reply_to: Option<i64>, _markup: &Value) -> Result<i64> {
        self.send_message(chat_id, text, reply_to)
    }
    /// `answerCallbackQuery` with a short toast. Default: nothing (for wrappers).
    fn answer_callback(&self, _callback_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }
    /// `editMessageText` (plain) with a new inline keyboard. Default: nothing (for wrappers).
    fn edit_message(&self, _chat_id: i64, _message_id: i64, _text: &str, _markup: &Value) -> Result<()> {
        Ok(())
    }
    /// `editMessageText` (plain) with no reply markup, which also removes any keyboard. Default: nothing.
    fn edit_text(&self, _chat_id: i64, _message_id: i64, _text: &str) -> Result<()> {
        Ok(())
    }
    /// `pinChatMessage` without a notification. Default: nothing (for wrappers).
    fn pin_message(&self, _chat_id: i64, _message_id: i64) -> Result<()> {
        Ok(())
    }
    /// `unpinChatMessage` for one message. Default: nothing (for wrappers).
    fn unpin_message(&self, _chat_id: i64, _message_id: i64) -> Result<()> {
        Ok(())
    }
    /// `deleteMessage`. Default: nothing (for wrappers).
    fn delete_message(&self, _chat_id: i64, _message_id: i64) -> Result<()> {
        Ok(())
    }
    /// `sendPhoto` / `sendDocument` (multipart upload of the file at `path`). `caption` may be empty;
    /// `html` sets `parse_mode` HTML. Returns the new message id. Default: unsupported (for wrappers).
    fn send_file(
        &self,
        _chat_id: i64,
        _kind: FileKind,
        _path: &Path,
        _caption: &str,
        _html: bool,
        _reply_to: Option<i64>,
    ) -> Result<i64> {
        Err(anyhow!("sending files is not supported by this client"))
    }
    /// `getFile`: where a file can be downloaded from. Default: unsupported (for wrappers).
    fn get_file(&self, _file_id: &str) -> Result<FileInfo> {
        Err(anyhow!("downloading files is not supported by this client"))
    }
    /// Downloads the file at `file_path` (from `get_file`) into `dest` (new file, mode 600), at most
    /// `max_bytes`. The download URL contains the token and is never logged. Default: unsupported.
    fn download_file(&self, _file_path: &str, _dest: &Path, _max_bytes: u64) -> Result<()> {
        Err(anyhow!("downloading files is not supported by this client"))
    }
    /// Registers the `/` command menu (`setMyCommands`): `(command, description)` pairs.
    fn set_my_commands(&self, commands: &[(&str, &str)]) -> Result<()>;
}

/// `setMyCommands` body.
pub fn set_my_commands_body(commands: &[(&str, &str)]) -> Value {
    json!({
        "commands": commands
            .iter()
            .map(|(c, d)| json!({ "command": c, "description": d }))
            .collect::<Vec<_>>(),
    })
}

/// `sendMessage` body. Plain text: no `parse_mode` (FR-41).
pub fn send_message_body(chat_id: i64, text: &str, reply_to: Option<i64>) -> Value {
    let mut body = json!({ "chat_id": chat_id, "text": text });
    if let Some(id) = reply_to {
        body["reply_parameters"] = json!({
            "message_id": id,
            "allow_sending_without_reply": true,
        });
    }
    body
}

/// `sendMessage` body with an inline keyboard.
pub fn send_keyboard_body(chat_id: i64, text: &str, reply_to: Option<i64>, markup: &Value) -> Value {
    let mut body = send_message_body(chat_id, text, reply_to);
    body["reply_markup"] = markup.clone();
    body
}

/// `answerCallbackQuery` body.
pub fn answer_callback_body(callback_id: &str, text: &str) -> Value {
    json!({ "callback_query_id": callback_id, "text": text })
}

/// `editMessageText` body (plain text) with a new inline keyboard.
pub fn edit_message_body(chat_id: i64, message_id: i64, text: &str, markup: &Value) -> Value {
    json!({ "chat_id": chat_id, "message_id": message_id, "text": text, "reply_markup": markup })
}

/// `editMessageText` body (plain text) without reply markup.
pub fn edit_text_body(chat_id: i64, message_id: i64, text: &str) -> Value {
    json!({ "chat_id": chat_id, "message_id": message_id, "text": text })
}

/// `pinChatMessage` body: pins silently.
pub fn pin_message_body(chat_id: i64, message_id: i64) -> Value {
    json!({ "chat_id": chat_id, "message_id": message_id, "disable_notification": true })
}

/// `unpinChatMessage` and `deleteMessage` body.
pub fn message_ref_body(chat_id: i64, message_id: i64) -> Value {
    json!({ "chat_id": chat_id, "message_id": message_id })
}

/// `sendMessage` body with `parse_mode` HTML.
pub fn send_html_body(chat_id: i64, html: &str, reply_to: Option<i64>) -> Value {
    let mut body = send_message_body(chat_id, html, reply_to);
    body["parse_mode"] = json!("HTML");
    body
}

/// Parses `{"ok":true,"result":...}`; `{"ok":false,"description":...}` is an error.
fn parse_response<T: DeserializeOwned>(body: &str) -> Result<T> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| anyhow!("unexpected (non-JSON) answer from Telegram"))?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        let desc = v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(anyhow!("Telegram API error: {desc}"));
    }
    let result = v
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("Telegram answer has no result"))?;
    serde_json::from_value(result).map_err(|e| anyhow!("unexpected Telegram answer: {e}"))
}

/// Removes the token from a message.
fn scrub_token(msg: &str, token: &str) -> String {
    if token.is_empty() {
        msg.to_string()
    } else {
        msg.replace(token, "<token>")
    }
}

/// Real client: `https://api.telegram.org/bot<token>/<method>`.
pub struct HttpTelegram {
    token: String,
    client: reqwest::blocking::Client,
}

impl HttpTelegram {
    pub fn new(token: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .build()
            .map_err(|e| anyhow!("could not build the HTTP client: {}", e.without_url()))?;
        Ok(Self {
            token: token.to_string(),
            client,
        })
    }

    fn call<T: DeserializeOwned>(&self, method: &str, body: &Value, timeout: Duration) -> Result<T> {
        let rb = self.post(method).timeout(timeout).json(body);
        self.finish(method, rb)
    }

    fn post(&self, method: &str) -> reqwest::blocking::RequestBuilder {
        self.client.post(format!("{API_BASE}/bot{}/{method}", self.token))
    }

    fn finish<T: DeserializeOwned>(&self, method: &str, rb: reqwest::blocking::RequestBuilder) -> Result<T> {
        let resp = rb.send().map_err(|e| {
            // reqwest errors carry the URL (and so the token): strip it.
            let msg = e.without_url().to_string();
            anyhow!("{method} request failed: {}", scrub_token(&msg, &self.token))
        })?;
        // Telegram answers errors (401, 400, ...) with a JSON body: parse it whatever the status.
        let status = resp.status();
        let text = resp.text().map_err(|e| {
            let msg = e.without_url().to_string();
            anyhow!("{method}: could not read the answer: {}", scrub_token(&msg, &self.token))
        })?;
        parse_response(&text).map_err(|e| {
            anyhow!(
                "{method} (HTTP {}): {}",
                status.as_u16(),
                scrub_token(&e.to_string(), &self.token)
            )
        })
    }
}

impl TelegramApi for HttpTelegram {
    fn get_me(&self) -> Result<BotInfo> {
        self.call("getMe", &json!({}), SHORT_TIMEOUT)
    }

    fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>> {
        let body = json!({
            "offset": offset,
            "timeout": timeout_secs,
            "allowed_updates": ALLOWED_UPDATES,
        });
        // HTTP timeout = long-poll timeout + 10 s.
        self.call("getUpdates", &body, Duration::from_secs(timeout_secs + 10))
    }

    fn send_message(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> Result<i64> {
        let s: Sent = self.call(
            "sendMessage",
            &send_message_body(chat_id, text, reply_to),
            SHORT_TIMEOUT,
        )?;
        Ok(s.message_id)
    }

    fn send_html(&self, chat_id: i64, html: &str, reply_to: Option<i64>) -> Result<i64> {
        let s: Sent = self.call("sendMessage", &send_html_body(chat_id, html, reply_to), SHORT_TIMEOUT)?;
        Ok(s.message_id)
    }

    fn send_keyboard(&self, chat_id: i64, text: &str, reply_to: Option<i64>, markup: &Value) -> Result<i64> {
        let s: Sent = self.call(
            "sendMessage",
            &send_keyboard_body(chat_id, text, reply_to, markup),
            SHORT_TIMEOUT,
        )?;
        Ok(s.message_id)
    }

    fn answer_callback(&self, callback_id: &str, text: &str) -> Result<()> {
        let _: Value = self.call("answerCallbackQuery", &answer_callback_body(callback_id, text), SHORT_TIMEOUT)?;
        Ok(())
    }

    fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, markup: &Value) -> Result<()> {
        let _: Value = self.call(
            "editMessageText",
            &edit_message_body(chat_id, message_id, text, markup),
            SHORT_TIMEOUT,
        )?;
        Ok(())
    }

    fn edit_text(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        let _: Value = self.call("editMessageText", &edit_text_body(chat_id, message_id, text), SHORT_TIMEOUT)?;
        Ok(())
    }

    fn pin_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        let _: Value = self.call("pinChatMessage", &pin_message_body(chat_id, message_id), SHORT_TIMEOUT)?;
        Ok(())
    }

    fn unpin_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        let _: Value = self.call("unpinChatMessage", &message_ref_body(chat_id, message_id), SHORT_TIMEOUT)?;
        Ok(())
    }

    fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        let _: Value = self.call("deleteMessage", &message_ref_body(chat_id, message_id), SHORT_TIMEOUT)?;
        Ok(())
    }

    fn send_file(
        &self,
        chat_id: i64,
        kind: FileKind,
        path: &Path,
        caption: &str,
        html: bool,
        reply_to: Option<i64>,
    ) -> Result<i64> {
        let data = std::fs::read(path).map_err(|e| anyhow!("could not read {}: {e}", path.display()))?;
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let chat = chat_id.to_string();
        let reply = json!({ "message_id": reply_to, "allow_sending_without_reply": true }).to_string();
        let mut fields: Vec<(&str, &str)> = vec![("chat_id", &chat)];
        if !caption.is_empty() {
            fields.push(("caption", caption));
            if html {
                fields.push(("parse_mode", "HTML"));
            }
        }
        if reply_to.is_some() {
            fields.push(("reply_parameters", &reply));
        }
        let boundary = pick_boundary(&data);
        let body = multipart_body(&boundary, &fields, kind.field(), filename, guess_content_type(path), &data);
        let rb = self
            .post(kind.method())
            .timeout(UPLOAD_TIMEOUT)
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .body(body);
        let s: Sent = self.finish(kind.method(), rb)?;
        Ok(s.message_id)
    }

    fn get_file(&self, file_id: &str) -> Result<FileInfo> {
        self.call("getFile", &json!({ "file_id": file_id }), SHORT_TIMEOUT)
    }

    fn download_file(&self, file_path: &str, dest: &Path, max_bytes: u64) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let url = format!("{API_BASE}/file/bot{}/{}", self.token, file_path.trim_start_matches('/'));
        let scrub = |m: String| scrub_token(&m, &self.token);
        let resp = self
            .client
            .get(url)
            .timeout(UPLOAD_TIMEOUT)
            .send()
            .map_err(|e| anyhow!("file download failed: {}", scrub(e.without_url().to_string())))?;
        if !resp.status().is_success() {
            return Err(anyhow!("file download failed (HTTP {})", resp.status().as_u16()));
        }
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(anyhow!("file too big"));
        }
        let bytes = resp
            .bytes()
            .map_err(|e| anyhow!("file download failed: {}", scrub(e.without_url().to_string())))?;
        if bytes.len() as u64 > max_bytes {
            return Err(anyhow!("file too big"));
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dest)
            .map_err(|e| anyhow!("could not create {}: {e}", dest.display()))?;
        f.write_all(&bytes).map_err(|e| anyhow!("could not write {}: {e}", dest.display()))
    }

    fn set_my_commands(&self, commands: &[(&str, &str)]) -> Result<()> {
        let _: Value = self.call("setMyCommands", &set_my_commands_body(commands), SHORT_TIMEOUT)?;
        Ok(())
    }
}

/// Stands in when the HTTP client cannot be built: every call reports why.
pub struct FailingTelegram(pub String);

impl TelegramApi for FailingTelegram {
    fn get_me(&self) -> Result<BotInfo> {
        Err(anyhow!("{}", self.0))
    }
    fn get_updates(&self, _offset: i64, _timeout_secs: u64) -> Result<Vec<Update>> {
        Err(anyhow!("{}", self.0))
    }
    fn send_message(&self, _chat_id: i64, _text: &str, _reply_to: Option<i64>) -> Result<i64> {
        Err(anyhow!("{}", self.0))
    }
    fn send_html(&self, _chat_id: i64, _html: &str, _reply_to: Option<i64>) -> Result<i64> {
        Err(anyhow!("{}", self.0))
    }
    fn send_keyboard(&self, _chat_id: i64, _text: &str, _reply_to: Option<i64>, _markup: &Value) -> Result<i64> {
        Err(anyhow!("{}", self.0))
    }
    fn answer_callback(&self, _callback_id: &str, _text: &str) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
    fn edit_message(&self, _chat_id: i64, _message_id: i64, _text: &str, _markup: &Value) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
    fn edit_text(&self, _chat_id: i64, _message_id: i64, _text: &str) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
    fn pin_message(&self, _chat_id: i64, _message_id: i64) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
    fn unpin_message(&self, _chat_id: i64, _message_id: i64) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
    fn delete_message(&self, _chat_id: i64, _message_id: i64) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
    fn send_file(&self, _c: i64, _k: FileKind, _p: &Path, _cap: &str, _h: bool, _r: Option<i64>) -> Result<i64> {
        Err(anyhow!("{}", self.0))
    }
    fn set_my_commands(&self, _commands: &[(&str, &str)]) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
}

/// A file the fake accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeFile {
    pub chat_id: i64,
    pub kind: FileKind,
    pub path: PathBuf,
    pub caption: String,
    pub html: bool,
    pub reply_to: Option<i64>,
}

/// In-memory API for tests.
pub struct FakeTelegram {
    /// `(chat_id, text, reply_to)` per `send_message` call.
    pub sent: RefCell<Vec<(i64, String, Option<i64>)>>,
    pub updates: RefCell<VecDeque<Vec<Update>>>,
    pub me: Result<BotInfo>,
    pub fail_updates: Cell<bool>,
    pub fail_send: Cell<bool>,
    /// Makes `send_html` fail like Telegram's "can't parse entities".
    pub reject_html: Cell<bool>,
    /// `(chat_id, html, reply_to)` per accepted `send_html` call (also in `sent`).
    pub html_sent: RefCell<Vec<(i64, String, Option<i64>)>>,
    /// Offsets passed to `get_updates`.
    pub offsets: RefCell<Vec<i64>>,
    /// `(command, description)` pairs per `set_my_commands` call.
    pub commands: RefCell<Vec<Vec<(String, String)>>>,
    pub fail_commands: Cell<bool>,
    /// Last message id handed out; each send returns the next one.
    pub next_id: Cell<i64>,
    /// `(message_id, chat_id, text)` per `send_message`/`send_html`/`send_keyboard`.
    pub ids: RefCell<Vec<(i64, i64, String)>>,
    /// `(chat_id, text, markup)` per `send_keyboard` call (also in `sent`).
    pub keyboards: RefCell<Vec<(i64, String, Value)>>,
    /// `(callback_id, text)` per `answer_callback` call.
    pub answers: RefCell<Vec<(String, String)>>,
    /// `(chat_id, message_id, text, markup)` per `edit_message` call.
    pub edits: RefCell<Vec<(i64, i64, String, Value)>>,
    /// `(chat_id, message_id, text)` per accepted `edit_text` call.
    pub text_edits: RefCell<Vec<(i64, i64, String)>>,
    /// `(chat_id, message_id)` per accepted `pin_message` call.
    pub pins: RefCell<Vec<(i64, i64)>>,
    /// `(chat_id, message_id)` per accepted `unpin_message` call.
    pub unpins: RefCell<Vec<(i64, i64)>>,
    /// `(chat_id, message_id)` per accepted `delete_message` call.
    pub deletes: RefCell<Vec<(i64, i64)>>,
    pub fail_pin: Cell<bool>,
    pub fail_delete: Cell<bool>,
    /// One entry per accepted `send_file` call (also in `ids`).
    pub files: RefCell<Vec<FakeFile>>,
    /// `allowed_updates` the poller asked for is fixed; the fake just counts polls.
    pub polls: Cell<usize>,
    /// Downloadable files by `file_id`: `(file_path, bytes)`; `file_size` is the byte length.
    pub downloads: RefCell<std::collections::HashMap<String, (String, Vec<u8>)>>,
    /// `file_id` per `get_file` call.
    pub file_requests: RefCell<Vec<String>>,
}

impl FakeTelegram {
    pub fn new() -> Self {
        Self {
            sent: RefCell::new(Vec::new()),
            updates: RefCell::new(VecDeque::new()),
            me: Ok(BotInfo {
                id: 111,
                username: "colony_bot".to_string(),
            }),
            fail_updates: Cell::new(false),
            fail_send: Cell::new(false),
            reject_html: Cell::new(false),
            html_sent: RefCell::new(Vec::new()),
            offsets: RefCell::new(Vec::new()),
            commands: RefCell::new(Vec::new()),
            fail_commands: Cell::new(false),
            next_id: Cell::new(1000),
            ids: RefCell::new(Vec::new()),
            keyboards: RefCell::new(Vec::new()),
            answers: RefCell::new(Vec::new()),
            edits: RefCell::new(Vec::new()),
            polls: Cell::new(0),
            text_edits: RefCell::new(Vec::new()),
            pins: RefCell::new(Vec::new()),
            unpins: RefCell::new(Vec::new()),
            deletes: RefCell::new(Vec::new()),
            fail_pin: Cell::new(false),
            fail_delete: Cell::new(false),
            files: RefCell::new(Vec::new()),
            downloads: RefCell::new(std::collections::HashMap::new()),
            file_requests: RefCell::new(Vec::new()),
        }
    }

    /// Makes `file_id` downloadable as `file_path` with `bytes`.
    pub fn add_download(&self, file_id: &str, file_path: &str, bytes: &[u8]) {
        self.downloads.borrow_mut().insert(file_id.to_string(), (file_path.to_string(), bytes.to_vec()));
    }

    /// A fake whose `get_me` fails with `msg`.
    pub fn rejecting(msg: &str) -> Self {
        Self {
            me: Err(anyhow!("{msg}")),
            ..Self::new()
        }
    }
}

impl Default for FakeTelegram {
    fn default() -> Self {
        Self::new()
    }
}

impl TelegramApi for FakeTelegram {
    fn get_me(&self) -> Result<BotInfo> {
        match &self.me {
            Ok(b) => Ok(b.clone()),
            Err(e) => Err(anyhow!("{e}")),
        }
    }

    fn get_updates(&self, offset: i64, _timeout_secs: u64) -> Result<Vec<Update>> {
        self.offsets.borrow_mut().push(offset);
        self.polls.set(self.polls.get() + 1);
        if self.fail_updates.get() {
            return Err(anyhow!("fake getUpdates failure"));
        }
        Ok(self.updates.borrow_mut().pop_front().unwrap_or_default())
    }

    fn send_message(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> Result<i64> {
        if self.fail_send.get() {
            return Err(anyhow!("fake sendMessage failure"));
        }
        self.sent
            .borrow_mut()
            .push((chat_id, text.to_string(), reply_to));
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        self.ids.borrow_mut().push((id, chat_id, text.to_string()));
        Ok(id)
    }

    fn send_html(&self, chat_id: i64, html: &str, reply_to: Option<i64>) -> Result<i64> {
        if self.reject_html.get() {
            return Err(anyhow!("Telegram API error: Bad Request: can't parse entities"));
        }
        let id = self.send_message(chat_id, html, reply_to)?;
        self.html_sent.borrow_mut().push((chat_id, html.to_string(), reply_to));
        Ok(id)
    }

    fn send_keyboard(&self, chat_id: i64, text: &str, reply_to: Option<i64>, markup: &Value) -> Result<i64> {
        let id = self.send_message(chat_id, text, reply_to)?;
        self.keyboards.borrow_mut().push((chat_id, text.to_string(), markup.clone()));
        Ok(id)
    }

    fn answer_callback(&self, callback_id: &str, text: &str) -> Result<()> {
        self.answers.borrow_mut().push((callback_id.to_string(), text.to_string()));
        Ok(())
    }

    fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, markup: &Value) -> Result<()> {
        self.edits.borrow_mut().push((chat_id, message_id, text.to_string(), markup.clone()));
        Ok(())
    }

    fn edit_text(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        self.text_edits.borrow_mut().push((chat_id, message_id, text.to_string()));
        Ok(())
    }

    fn pin_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        if self.fail_pin.get() {
            return Err(anyhow!("fake pinChatMessage failure"));
        }
        self.pins.borrow_mut().push((chat_id, message_id));
        Ok(())
    }

    fn unpin_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        self.unpins.borrow_mut().push((chat_id, message_id));
        Ok(())
    }

    fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<()> {
        if self.fail_delete.get() {
            return Err(anyhow!("Telegram API error: Bad Request: message can't be deleted"));
        }
        self.deletes.borrow_mut().push((chat_id, message_id));
        Ok(())
    }

    fn send_file(
        &self,
        chat_id: i64,
        kind: FileKind,
        path: &Path,
        caption: &str,
        html: bool,
        reply_to: Option<i64>,
    ) -> Result<i64> {
        if self.fail_send.get() {
            return Err(anyhow!("fake {} failure", kind.method()));
        }
        if html && self.reject_html.get() {
            return Err(anyhow!("Telegram API error: Bad Request: can't parse entities"));
        }
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        self.ids.borrow_mut().push((id, chat_id, caption.to_string()));
        self.files.borrow_mut().push(FakeFile {
            chat_id,
            kind,
            path: path.to_path_buf(),
            caption: caption.to_string(),
            html,
            reply_to,
        });
        Ok(id)
    }

    fn get_file(&self, file_id: &str) -> Result<FileInfo> {
        self.file_requests.borrow_mut().push(file_id.to_string());
        match self.downloads.borrow().get(file_id) {
            Some((p, b)) => Ok(FileInfo { file_path: Some(p.clone()), file_size: Some(b.len() as u64) }),
            None => Err(anyhow!("Telegram API error: Bad Request: wrong file_id")),
        }
    }

    fn download_file(&self, file_path: &str, dest: &Path, max_bytes: u64) -> Result<()> {
        let map = self.downloads.borrow();
        let Some((_, bytes)) = map.values().find(|(p, _)| p == file_path) else {
            return Err(anyhow!("file download failed (HTTP 404)"));
        };
        if bytes.len() as u64 > max_bytes {
            return Err(anyhow!("file too big"));
        }
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(dest)?;
        std::io::Write::write_all(&mut f, bytes)?;
        Ok(())
    }

    fn set_my_commands(&self, commands: &[(&str, &str)]) -> Result<()> {
        if self.fail_commands.get() {
            return Err(anyhow!("fake setMyCommands failure"));
        }
        self.commands.borrow_mut().push(
            commands
                .iter()
                .map(|(c, d)| (c.to_string(), d.to_string()))
                .collect(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_body_format() {
        let body = multipart_body(
            "BND",
            &[("chat_id", "42"), ("caption", "hi \"x\"")],
            "photo",
            "a\"b\r\n.png",
            "image/png",
            &[1, 2, 255],
        );
        let mut expect = Vec::new();
        expect.extend_from_slice(
            b"--BND\r\nContent-Disposition: form-data; name=\"chat_id\"\r\n\r\n42\r\n\
--BND\r\nContent-Disposition: form-data; name=\"caption\"\r\n\r\nhi \"x\"\r\n\
--BND\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"a_b__.png\"\r\nContent-Type: image/png\r\n\r\n",
        );
        expect.extend_from_slice(&[1, 2, 255]);
        expect.extend_from_slice(b"\r\n--BND--\r\n");
        assert_eq!(body, expect);
    }

    #[test]
    fn content_type_from_extension() {
        for (f, t) in [
            ("a.jpg", "image/jpeg"),
            ("a.JPEG", "image/jpeg"),
            ("a.png", "image/png"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.pdf", "application/octet-stream"),
            ("noext", "application/octet-stream"),
        ] {
            assert_eq!(guess_content_type(Path::new(f)), t, "{f}");
        }
    }

    #[test]
    fn boundary_avoids_data() {
        let b = pick_boundary(b"plain");
        assert!(b.starts_with("----colony"));
    }

    #[test]
    fn body_has_no_parse_mode() {
        let plain = send_message_body(42, "hi", None);
        let mut keys: Vec<&str> = plain.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["chat_id", "text"]);
        let replied = send_message_body(42, "hi", Some(7));
        let mut keys: Vec<&str> = replied.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["chat_id", "reply_parameters", "text"]);
    }

    #[test]
    fn html_body_has_parse_mode() {
        let b = send_html_body(42, "<b>x</b>", Some(7));
        assert_eq!(b["parse_mode"], "HTML");
        assert_eq!(b["text"], "<b>x</b>");
        assert_eq!(b["reply_parameters"]["message_id"], 7);
    }

    #[test]
    fn set_my_commands_body_shape() {
        let b = set_my_commands_body(&[("list", "List"), ("use", "Use")]);
        assert_eq!(
            b,
            json!({"commands":[{"command":"list","description":"List"},{"command":"use","description":"Use"}]})
        );
    }

    #[test]
    fn allowed_updates_include_callback_query() {
        assert_eq!(ALLOWED_UPDATES, ["message", "callback_query"]);
    }

    #[test]
    fn keyboard_edit_answer_bodies() {
        let kb = json!({"inline_keyboard":[[{"text":"a","callback_data":"use:x"}]]});
        let b = send_keyboard_body(1, "t", Some(2), &kb);
        assert_eq!(b["reply_markup"], kb);
        assert_eq!(b["reply_parameters"]["message_id"], 2);
        let e = edit_message_body(1, 9, "t", &kb);
        assert_eq!(e["message_id"], 9);
        assert_eq!(e["reply_markup"], kb);
        assert_eq!(answer_callback_body("cb", "hi"), json!({"callback_query_id":"cb","text":"hi"}));
    }

    #[test]
    fn callback_and_reply_to_parse() {
        let cq = r#"{"update_id":7,"callback_query":{"id":"abc","from":{"id":42,"is_bot":false},"message":{"message_id":5,"chat":{"id":42,"type":"private"},"date":1},"chat_instance":"x","data":"use:w1:p2"}}"#;
        let u: Update = serde_json::from_str(cq).unwrap();
        let c = u.callback_query.unwrap();
        assert_eq!((c.id.as_str(), c.from.id, c.data.as_deref()), ("abc", 42, Some("use:w1:p2")));
        assert_eq!(c.message.unwrap().message_id, 5);
        let m = r#"{"update_id":8,"message":{"message_id":6,"from":{"id":42},"chat":{"id":42,"type":"private"},"date":2,"text":"x","reply_to_message":{"message_id":77,"chat":{"id":42,"type":"private"},"date":1}}}"#;
        let u: Update = serde_json::from_str(m).unwrap();
        assert_eq!(u.message.unwrap().reply_to_message.unwrap().message_id, 77);
    }

    #[test]
    fn body_keeps_markdown_chars() {
        let text = "*_`[ <b>x</b> $HOME";
        let body = send_message_body(1, text, None);
        assert_eq!(body["text"], text);
    }

    #[test]
    fn body_reply_parameters() {
        let body = send_message_body(42, "hi", Some(7));
        assert_eq!(
            body["reply_parameters"],
            json!({"message_id": 7, "allow_sending_without_reply": true})
        );
        assert_eq!(body["chat_id"], 42);
    }

    #[test]
    fn update_parses_private_and_group() {
        let private = r#"{"update_id":10000,"message":{"message_id":1365,"from":{"id":1111111,"is_bot":false,"first_name":"Test"},"chat":{"id":1111111,"first_name":"Test","type":"private"},"date":1441645532,"text":"/start"}}"#;
        let u: Update = serde_json::from_str(private).unwrap();
        assert_eq!(u.update_id, 10000);
        let m = u.message.unwrap();
        assert_eq!(m.message_id, 1365);
        assert_eq!(m.from.unwrap().id, 1111111);
        assert_eq!(m.chat.kind, "private");
        assert_eq!(m.text.as_deref(), Some("/start"));

        let group = r#"{"update_id":10001,"message":{"message_id":1366,"from":{"id":1111111,"is_bot":false,"first_name":"Test"},"chat":{"id":-1001234567890,"title":"Team","type":"supergroup"},"date":1441645533,"text":"hello"}}"#;
        let u: Update = serde_json::from_str(group).unwrap();
        let m = u.message.unwrap();
        assert_eq!(m.chat.id, -1001234567890);
        assert_eq!(m.chat.kind, "supergroup");

        // Updates without a message (edited posts, etc.) still parse.
        let other: Update = serde_json::from_str(r#"{"update_id":5,"edited_message":{}}"#).unwrap();
        assert!(other.message.is_none());
    }

    #[test]
    fn api_error_ok_false_is_err() {
        let token = "123456:SECRET-token";
        let err = parse_response::<BotInfo>(r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#)
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("Unauthorized"), "{text}");
        assert!(!text.contains(token));
        // Even a description that echoes the token is scrubbed on the HTTP path.
        let leaked = scrub_token("bad https://api.telegram.org/bot123456:SECRET-token/getMe", token);
        assert!(!leaked.contains(token), "{leaked}");
    }

    #[test]
    fn ok_true_result_parses() {
        let me: BotInfo =
            parse_response(r#"{"ok":true,"result":{"id":99,"is_bot":true,"username":"colony_bot"}}"#).unwrap();
        assert_eq!(me, BotInfo { id: 99, username: "colony_bot".to_string() });
    }
}
