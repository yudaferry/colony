//! Telegram Bot API client (blocking). The bot token is part of every URL, so
//! it is scrubbed from every error text and never printed or logged.

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Duration;

const API_BASE: &str = "https://api.telegram.org";
/// Update kinds the bot asks for.
pub const ALLOWED_UPDATES: [&str; 2] = ["message", "callback_query"];
const SHORT_TIMEOUT: Duration = Duration::from_secs(20);

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

/// Answer of `sendMessage`: only the new message's id.
#[derive(Deserialize, Debug)]
struct Sent {
    message_id: i64,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
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
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
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
        let url = format!("{API_BASE}/bot{}/{method}", self.token);
        let resp = self
            .client
            .post(&url)
            .timeout(timeout)
            .json(body)
            .send()
            .map_err(|e| {
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
    fn set_my_commands(&self, _commands: &[(&str, &str)]) -> Result<()> {
        Err(anyhow!("{}", self.0))
    }
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
    /// `allowed_updates` the poller asked for is fixed; the fake just counts polls.
    pub polls: Cell<usize>,
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
        }
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
