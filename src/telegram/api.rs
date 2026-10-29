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
    fn send_message(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> Result<()>;
}

/// `sendMessage` body. Plain text only: no `parse_mode` (FR-41).
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
            "allowed_updates": ["message"],
        });
        // HTTP timeout = long-poll timeout + 10 s.
        self.call("getUpdates", &body, Duration::from_secs(timeout_secs + 10))
    }

    fn send_message(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> Result<()> {
        let _: Value = self.call(
            "sendMessage",
            &send_message_body(chat_id, text, reply_to),
            SHORT_TIMEOUT,
        )?;
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
    fn send_message(&self, _chat_id: i64, _text: &str, _reply_to: Option<i64>) -> Result<()> {
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
    /// Offsets passed to `get_updates`.
    pub offsets: RefCell<Vec<i64>>,
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
            offsets: RefCell::new(Vec::new()),
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
        if self.fail_updates.get() {
            return Err(anyhow!("fake getUpdates failure"));
        }
        Ok(self.updates.borrow_mut().pop_front().unwrap_or_default())
    }

    fn send_message(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> Result<()> {
        if self.fail_send.get() {
            return Err(anyhow!("fake sendMessage failure"));
        }
        self.sent
            .borrow_mut()
            .push((chat_id, text.to_string(), reply_to));
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
