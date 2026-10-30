//! The bot: a pure update handler (`handle_update`) and a thin polling loop.
//! Slash commands call core functions directly; no Claude session is involved (AG-4).
//! Everything sent to Telegram is an answer to a locked-user message or a reply
//! carrying a reply ID (FR-44, D-20).

use super::cmds::bot_id_of;
use super::inbox;
use super::sched;
use chrono::{DateTime, Utc};
use super::api::{CallbackQuery, Message, TelegramApi, Update};
use super::reply_id::{PENDING_MODE, create_pending};
use super::split::{TG_LIMIT, split_message};
use crate::cli::Origin;
use crate::commands::list::{ListRow, list_rows};
use crate::commands::list::UNREGISTERED_NOTE;
use crate::commands::{backend_str, send::send};
use crate::ctx::Ctx;
use crate::discover::resolve;
use crate::state::names::{NAMES_FILE, Names};
use crate::state::pending::{PENDING_FILE, Pending};
use crate::state::sent::{SENT_FILE, Sent};
use crate::state::telegram::{TELEGRAM_FILE, TELEGRAM_MODE, TelegramConfig};
use crate::time::wib_secs;
use anyhow::Result;
use serde_json::{Value, json};
use std::time::Duration;

const POLL_TIMEOUT_SECS: u64 = 30;
const RETRY_SLEEP: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotReply {
    pub chat_id: i64,
    pub reply_to: i64,
    pub text: String,
    /// Inline keyboard (`reply_markup`) sent with the text.
    pub markup: Option<Value>,
}

/// What to do about a button tap: answer the query, and refresh the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackReply {
    pub callback_id: String,
    pub toast: String,
    /// `(chat_id, message_id, text, markup)` for `editMessageText`; no markup removes the keyboard.
    pub edit: Option<(i64, i64, String, Option<Value>)>,
}

const ICON_SENT: &str = "\u{1f4e8}";
const ICON_FILE: &str = "\u{1f4ce}";
const USE_PROMPT: &str = "\u{1f3af} Choose default session:";
/// Telegram's limit for `callback_data`.
const CALLBACK_MAX: usize = 64;

/// Answer to any `/word` other than `/list` and `/use`.
pub const ONLY_HINT: &str = "\u{2139}\u{fe0f} only /list and /use \u{2014} type normally to talk to your session";

/// The `/` menu registered with `setMyCommands`.
pub const BOT_COMMANDS: [(&str, &str); 2] = [
    ("list", "List sessions"),
    ("use", "Set default session: /use (buttons) | <name> | off"),
];

/// Registers the command menu. A failure is logged to stderr and never stops the bot.
pub fn register_commands(ctx: &Ctx, api: &dyn TelegramApi) {
    if let Err(e) = api.set_my_commands(&BOT_COMMANDS) {
        // `e` comes from the API layer, which scrubs the token.
        warn(ctx, &format!("could not set the command menu: {e:#}"));
    }
}

/// Splits `s` at the first whitespace: (first word, trimmed rest).
pub(super) fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((w, rest)) => (w, rest.trim()),
        None => (s, ""),
    }
}

/// The listed session (registered or discovered, pane still there) called `name`,
/// else the one with pane id `name` (D-12). A failing backend query leaves entries
/// listed with status `?`.
fn find_row(ctx: &Ctx, name: &str) -> Result<Option<ListRow>> {
    let lname = name.to_lowercase();
    let mut rows = list_rows(ctx)?;
    let i = rows
        .iter()
        .position(|r| r.name.to_lowercase() == lname)
        .or_else(|| rows.iter().position(|r| r.pane_id.to_lowercase() == lname));
    Ok(i.map(|i| rows.swap_remove(i)))
}

/// Where an answer goes: `(chat_id, message_id to quote)`; message id 0 quotes nothing.
pub(crate) type Src = (i64, i64);

fn src(msg: &Message) -> Src {
    (msg.chat.id, msg.message_id)
}

/// Delivers `text` to the live session `row` and words the acknowledgement. A
/// session blocked on a prompt is refused before any reply ID exists.
fn deliver(ctx: &Ctx, src: Src, name: &str, row: &ListRow, text: &str, icon: &str) -> Result<String> {
    if row.status == "blocked" {
        return Ok(format!("\u{26d4} not delivered: {name} is waiting on a prompt"));
    }
    Ok(match forward(ctx, src, name, text)? {
        Ok(()) if row.status == "working" => {
            format!("{icon} sent to {name} (\u{23f3} busy \u{2014} it will answer after its current task)")
        }
        Ok(()) => format!("{icon} sent to {name}"),
        Err(e) => format!("\u{26a0}\u{fe0f} could not send to {name}: {e}"),
    })
}

/// Delivers a scheduled message to session `target` with a new reply id for `chat_id` (nothing to
/// quote). The acknowledgement starts with the scheduler icon on success, else says why not.
pub(super) fn deliver_scheduled(ctx: &Ctx, chat_id: i64, target: &str, text: &str) -> Result<String> {
    let Some(row) = find_row(ctx, target)? else {
        return Ok(format!("\u{26a0}\u{fe0f} no session named {}", target.to_lowercase()));
    };
    deliver(ctx, (chat_id, 0), &target.to_lowercase(), &row, text, super::sched::ICON_SCHED)
}

/// New reply ID, then deliver with a Telegram envelope. A failed delivery
/// removes the ID again.
fn forward(ctx: &Ctx, src: Src, name: &str, text: &str) -> Result<Result<(), String>> {
    let lname = name.to_lowercase();
    let id = create_pending(ctx.state, ctx.clock.now(), &lname, src.0, src.1)?;
    match send(ctx, text, name, false, Some(&Origin::Telegram(id.clone()))) {
        Ok(_) => Ok(Ok(())),
        Err(e) => {
            ctx.state.update_json(PENDING_FILE, PENDING_MODE, |p: &mut Pending| {
                p.0.remove(&id);
                Ok(())
            })?;
            Ok(Err(format!("{e:#}")))
        }
    }
}

/// Status marker for a `/list` row: idle, working, blocked; anything else is unknown.
fn status_dot(status: &str) -> &'static str {
    match status {
        "idle" => "\u{1f7e2}",
        "working" => "\u{1f7e1}",
        "blocked" => "\u{1f534}",
        _ => "\u{26aa}",
    }
}

fn cmd_list(ctx: &Ctx) -> Result<String> {
    let rows = list_rows(ctx)?;
    if rows.is_empty() {
        return Ok("\u{1f4ed} no sessions".to_string());
    }
    let any_unregistered = rows.iter().any(|r| !r.registered);
    let mut out = rows
        .iter()
        .map(|r| {
            format!(
                "{} {} - {} - {} ({} {})",
                status_dot(&r.status),
                r.name,
                r.kind_label(),
                r.status,
                backend_str(r.backend),
                r.pane_id
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if any_unregistered {
        out.push('\n');
        out.push_str(UNREGISTERED_NOTE);
    }
    Ok(out)
}

/// Sets the default session like `/use <arg>`: `Ok(lowercase name)` or `Err(message)`
/// when `arg` is not a usable session. Same setting as `colony telegram receiver`.
fn set_default(ctx: &Ctx, arg: &str) -> Result<std::result::Result<String, String>> {
    let lname = arg.to_lowercase();
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    if let Err(e) = resolve(ctx, arg, &names) {
        return Ok(Err(format!("\u{26a0}\u{fe0f} cannot use {lname}: {e:#}")));
    }
    if find_row(ctx, arg)?.is_none() {
        return Ok(Err(format!("\u{26a0}\u{fe0f} no session named {lname}")));
    }
    ctx.state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            c.receiver = Some(lname.clone());
            Ok(())
        })?;
    Ok(Ok(lname))
}

fn clear_default(ctx: &Ctx) -> Result<()> {
    ctx.state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            c.receiver = None;
            Ok(())
        })
}

/// `/use <name>` and `/use off`. (`/use` alone is the button keyboard, built in `handle_update`.)
fn cmd_use(ctx: &Ctx, args: &str) -> Result<String> {
    let arg = args.trim();
    if arg.eq_ignore_ascii_case("off") {
        clear_default(ctx)?;
        return Ok("\u{1f3af} default session cleared".to_string());
    }
    if arg.contains(char::is_whitespace) {
        return Ok("\u{26a0}\u{fe0f} usage: /use <name> | off".to_string());
    }
    Ok(match set_default(ctx, arg)? {
        Ok(lname) => format!("\u{1f3af} default session: {lname}"),
        Err(msg) => msg,
    })
}

/// `callback_data` of a session button: `use:<pane id>`, or `use:#<index>` when the
/// pane id would not fit Telegram's 64 bytes.
fn button_data(row: &ListRow, index: usize) -> String {
    let d = format!("use:{}", row.pane_id);
    if d.len() <= CALLBACK_MAX { d } else { format!("use:#{index}") }
}

/// The `/use` keyboard: one button per session (status dot + name, the default
/// marked with a check). No "off" button; `/use off` clears by typing.
fn use_keyboard(ctx: &Ctx) -> Result<Value> {
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    let current = cfg.receiver.as_deref().map(str::to_lowercase);
    let mut rows: Vec<Value> = list_rows(ctx)?
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let is_default = current
                .as_deref()
                .is_some_and(|c| c == r.name.to_lowercase() || c == r.pane_id.to_lowercase());
            let label = format!("{}{} {}", if is_default { "\u{2705} " } else { "" }, status_dot(&r.status), r.name);
            json!([{ "text": label, "callback_data": button_data(r, i) }])
        })
        .collect();
    Ok(json!({ "inline_keyboard": rows }))
}

/// Handles a button tap. `None` means: ignore (not the locked user, not a private
/// chat, not ours). The same lock rules as for messages; a tap never claims the lock.
pub fn handle_callback(ctx: &Ctx, u: &Update) -> Result<Option<CallbackReply>> {
    let Some(cq) = &u.callback_query else {
        return Ok(None);
    };
    let Some(msg) = &cq.message else {
        return Ok(None);
    };
    if msg.chat.kind != "private" {
        return Ok(None);
    }
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    if cfg.locked_user_id != Some(cq.from.id) {
        return Ok(None);
    }
    if cfg.locked_chat_id.is_some_and(|c| c != msg.chat.id) {
        return Ok(None);
    }
    let Some(data) = cq.data.as_deref().and_then(|d| d.strip_prefix("use:")) else {
        return Ok(None);
    };
    // `done` is the final text of the tapped message once the default changed.
    // `use:off` comes only from keyboards sent before the off button was removed.
    let (toast, done) = if data == "off" {
        clear_default(ctx)?;
        ("\u{1f3af} default cleared".to_string(), Some("\u{1f3af} Default session cleared".to_string()))
    } else {
        pick(ctx, data)?
    };
    Ok(Some(callback_reply(ctx, cq, msg, toast, done)?))
}

/// Applies a session button; returns the toast and, when the default changed, the
/// text the tapped message becomes.
fn pick(ctx: &Ctx, data: &str) -> Result<(String, Option<String>)> {
    let rows = list_rows(ctx)?;
    let row = match data.strip_prefix('#') {
        Some(i) => i.parse::<usize>().ok().and_then(|i| rows.get(i)),
        None => rows.iter().find(|r| r.pane_id == data),
    };
    let Some(row) = row else {
        return Ok(("\u{26a0}\u{fe0f} session gone".to_string(), None));
    };
    // The name, as `/use <name>` would; a discovered name shared by several
    // sessions is not usable, then the pane id is.
    let mut last = String::new();
    for arg in [row.name.as_str(), row.pane_id.as_str()] {
        match set_default(ctx, arg)? {
            Ok(lname) => {
                return Ok((format!("\u{1f3af} {lname}"), Some(format!("\u{1f3af} Default session: {lname}"))));
            }
            Err(msg) => last = msg,
        }
    }
    Ok((last, None))
}

/// A changed default turns the tapped message into plain text (no keyboard); a
/// failed pick keeps the buttons, refreshed.
fn callback_reply(
    ctx: &Ctx,
    cq: &CallbackQuery,
    msg: &Message,
    toast: String,
    done: Option<String>,
) -> Result<CallbackReply> {
    let (text, markup) = match done {
        Some(text) => (text, None),
        None => (USE_PROMPT.to_string(), Some(use_keyboard(ctx)?)),
    };
    Ok(CallbackReply {
        callback_id: cq.id.clone(),
        toast,
        edit: Some((msg.chat.id, msg.message_id, text, markup)),
    })
}

fn cmd_plain(ctx: &Ctx, msg: &Message, cfg: &TelegramConfig, text: &str, icon: &str) -> Result<String> {
    let Some(receiver) = cfg.receiver.as_deref() else {
        return Ok("\u{2753} no session selected, use /use <name>".to_string());
    };
    let Some(row) = find_row(ctx, receiver)? else {
        return Ok(format!("\u{26a0}\u{fe0f} receiver {receiver} not found, use /use"));
    };
    deliver(ctx, src(msg), receiver, &row, text, icon)
}

/// Plain text. A swipe-reply to a message `colony telegram reply` sent goes to the
/// session that sent it (default untouched); any other message, or a
/// reply to an unknown one, follows the default session.
fn cmd_reply_or_plain(ctx: &Ctx, msg: &Message, cfg: &TelegramConfig, text: &str, icon: &str) -> Result<String> {
    if let Some(r) = &msg.reply_to_message {
        let sent: Sent = ctx.state.read_json(SENT_FILE)?;
        if let Some(target) = sent.target_of(msg.chat.id, r.message_id, ctx.clock.now()) {
            let Some(row) = find_row(ctx, target)? else {
                return Ok(format!("\u{26a0}\u{fe0f} no session named {}", target.to_lowercase()));
            };
            return deliver(ctx, src(msg), &target.to_lowercase(), &row, text, icon);
        }
    }
    cmd_plain(ctx, msg, cfg, text, icon)
}

/// Result of the checks every message from the phone passes first.
enum Gate {
    /// Not for us: say nothing.
    Ignore,
    /// Answer this and do nothing else (the lock was just claimed).
    Say(BotReply),
    /// The locked user, in the private chat: carry on with this config.
    Open(TelegramConfig),
}

fn say_to(msg: &Message, text: String) -> BotReply {
    BotReply { chat_id: msg.chat.id, reply_to: msg.message_id, text, markup: None }
}

/// Private chat, known sender, not stale, and the lock (claimed by the first message).
fn gate(ctx: &Ctx, msg: &Message) -> Result<Gate> {
    if msg.chat.kind != "private" {
        return Ok(Gate::Ignore);
    }
    let Some(from) = &msg.from else {
        return Ok(Gate::Ignore);
    };
    // Stale updates: Telegram replays up to 24 h of backlog. Anything sent before
    // the last connect is ignored entirely and never executed (D-15). `date` has
    // one-second resolution, so a message in the exact connect second cannot be told
    // apart from one sent just before it: `<=` drops it. A legitimate message in
    // that very second is lost (acceptable); the user simply sends it again.
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    if let Some(t) = cfg.connected_at {
        if msg.date <= t {
            return Ok(Gate::Ignore);
        }
    }
    // Lock: the first private message after connect locks the bot to its sender
    // and is not run. Without `connected_at` nobody can claim the lock.
    let locked_user = match cfg.locked_user_id {
        Some(id) => id,
        None => {
            if cfg.connected_at.is_none() {
                return Ok(Gate::Ignore);
            }
            let (id, newly) =
                ctx.state
                    .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                        match c.locked_user_id {
                            Some(id) => Ok((id, false)),
                            None => {
                                c.locked_user_id = Some(from.id);
                                c.locked_chat_id = Some(msg.chat.id);
                                Ok((from.id, true))
                            }
                        }
                    })?;
            if newly {
                return Ok(Gate::Say(say_to(msg, "\u{1f512} locked to you, send /list".to_string())));
            }
            id
        }
    };
    if from.id != locked_user {
        return Ok(Gate::Ignore);
    }
    Ok(Gate::Open(cfg))
}

/// Handles one text update. `None` means: say nothing. (Photos and files: `handle_file`.)
pub fn handle_update(ctx: &Ctx, u: &Update) -> Result<Option<BotReply>> {
    let Some(msg) = &u.message else {
        return Ok(None);
    };
    let Some(text) = &msg.text else {
        return Ok(None);
    };
    let cfg = match gate(ctx, msg)? {
        Gate::Ignore => return Ok(None),
        Gate::Say(r) => return Ok(Some(r)),
        Gate::Open(cfg) => cfg,
    };
    let say = |text: String| say_to(msg, text);

    let trimmed = text.trim();
    let reply = if let Some(cmdline) = trimmed.strip_prefix('/') {
        let (word, args) = first_word(cmdline);
        // `/list@colony_bot` -> `list`
        let command = word.split('@').next().unwrap_or("").to_lowercase();
        match command.as_str() {
            "list" => cmd_list(ctx)?,
            "use" if args.trim().is_empty() => {
                let mut r = say(USE_PROMPT.to_string());
                r.markup = Some(use_keyboard(ctx)?);
                return Ok(Some(r));
            }
            "use" => cmd_use(ctx, args)?,
            _ => ONLY_HINT.to_string(),
        }
    } else if trimmed.is_empty() {
        return Ok(None);
    } else {
        cmd_reply_or_plain(ctx, msg, &cfg, trimmed, ICON_SENT)?
    };
    Ok(Some(say(reply)))
}

/// A photo, document, video or audio message: download it to the inbox, then route it
/// like text (swipe-reply target first, else the default session). No session: the
/// downloaded file is deleted again. `None` means: not a file message, or ignore.
pub fn handle_file(ctx: &Ctx, api: &dyn TelegramApi, u: &Update) -> Result<Option<BotReply>> {
    let Some(msg) = &u.message else {
        return Ok(None);
    };
    let Some((file, is_photo)) = msg.attachment() else {
        return Ok(None);
    };
    let cfg = match gate(ctx, msg)? {
        Gate::Ignore => return Ok(None),
        Gate::Say(r) => return Ok(Some(r)),
        Gate::Open(cfg) => cfg,
    };
    let path = match inbox::fetch(ctx, api, file, is_photo) {
        Ok(p) => p,
        Err(inbox::FetchError::TooBig) => return Ok(Some(say_to(msg, inbox::TOO_BIG.to_string()))),
        Err(inbox::FetchError::Failed(e)) => {
            return Ok(Some(say_to(msg, format!("\u{26a0}\u{fe0f} could not download the file: {e}"))));
        }
    };
    let caption = msg.caption.as_deref().map(str::trim).filter(|c| !c.is_empty()).unwrap_or("(no caption)");
    let text = format!("{caption}\nAttachment: {}", path.display());
    let reply = cmd_reply_or_plain(ctx, msg, &cfg, &text, ICON_FILE)?;
    if !reply.starts_with(ICON_FILE) {
        let _ = std::fs::remove_file(&path);
    }
    Ok(Some(say_to(msg, reply)))
}

/// Sends a reply through `split_message`, `reply_to` on the first chunk only.
pub fn send_reply(api: &dyn TelegramApi, r: &BotReply) -> Result<()> {
    if let Some(markup) = &r.markup {
        // A keyboard message is short: one message.
        api.send_keyboard(r.chat_id, &r.text, Some(r.reply_to), markup)?;
        return Ok(());
    }
    for (i, chunk) in split_message(&r.text, TG_LIMIT).iter().enumerate() {
        api.send_message(r.chat_id, chunk, (i == 0).then_some(r.reply_to))?;
    }
    Ok(())
}

/// Answers the tap (toast), then refreshes the message so the check moves. A refused
/// edit (e.g. "message is not modified") is harmless.
pub fn send_callback_reply(api: &dyn TelegramApi, r: &CallbackReply) -> Result<()> {
    api.answer_callback(&r.callback_id, &r.toast)?;
    if let Some((chat, mid, text, markup)) = &r.edit {
        let _ = match markup {
            Some(m) => api.edit_message(*chat, *mid, text, m),
            None => api.edit_text(*chat, *mid, text),
        };
    }
    Ok(())
}

/// Text of the pinned bar for the default session `receiver`.
pub fn pin_text(receiver: Option<&str>) -> String {
    format!("\u{1f4cc} CURRENT SESSION: {}", receiver.map_or("none".to_string(), str::to_uppercase))
}

/// Makes the pinned "current session" message show the current default. The Telegram
/// app's pinned bar does not follow edits, so a changed text means: unpin and delete the
/// stored message, send a new one, pin it silently, and store its id and text. An unchanged
/// text does nothing. Skips when not locked. Errors are for the caller to report; they
/// must never undo the default change.
pub fn sync_pin(ctx: &Ctx, api: &dyn TelegramApi) -> Result<()> {
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    let (Some(_), Some(chat)) = (cfg.locked_user_id, cfg.locked_chat_id) else {
        return Ok(());
    };
    let text = pin_text(cfg.receiver.as_deref());
    if cfg.pinned_message_id.is_some() && cfg.pinned_text.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    if let Some(old) = cfg.pinned_message_id {
        for (what, r) in [("unpin", api.unpin_message(chat, old)), ("delete", api.delete_message(chat, old))] {
            if let Err(e) = r {
                let m = format!("{e:#}").to_lowercase();
                let benign = m.contains("not found") || m.contains("can't be deleted") || m.contains("message_id_invalid");
                if !benign {
                    warn(ctx, &format!("could not {what} the old pinned bar: {e:#}"));
                }
            }
        }
    }
    let mid = api.send_message(chat, &text, None)?;
    ctx.state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            c.pinned_message_id = Some(mid);
            c.pinned_text = Some(text.clone());
            Ok(())
        })?;
    api.pin_message(chat, mid)
}

/// `sync_pin`, with a failure reported on stderr only.
pub fn sync_pin_warn(ctx: &Ctx, api: &dyn TelegramApi) {
    if let Err(e) = sync_pin(ctx, api) {
        warn(ctx, &format!("could not update the pinned session bar: {e:#}"));
    }
}

pub(super) fn warn(ctx: &Ctx, what: &str) {
    eprintln!("[{}] colony telegram: {what}", wib_secs(ctx.clock.now()));
}

/// Saves the next offset, only while the saved token still belongs to `bot`, the
/// bot the update was fetched from. A reconnect to another bot therefore never
/// receives the old bot's offset. Failure is reported, not fatal.
fn persist_offset(ctx: &Ctx, bot: Option<i64>, offset: i64) {
    let r = ctx
        .state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            let saved = c.token.as_deref().and_then(bot_id_of);
            if bot.is_some() && saved == bot {
                c.offset = offset;
                c.offset_bot_id = bot;
            }
            Ok(())
        });
    if let Err(e) = r {
        warn(ctx, &format!("could not save offset: {e:#}"));
    }
}

/// Aligns the in-memory offset with the bot of `token`. `current_bot` is the bot
/// the in-memory offset belongs to (`None` at loop start). When it differs, the
/// offset becomes the saved one if that was recorded for the new bot, else 0 (and
/// the saved offset is reset too). The old bot's offset is never kept or written.
fn sync_offset(
    ctx: &Ctx,
    cfg: &TelegramConfig,
    token: &str,
    offset: &mut i64,
    current_bot: &mut Option<i64>,
) -> Result<()> {
    let bot = bot_id_of(token);
    if bot.is_none() {
        *offset = 0;
        *current_bot = None;
        return Ok(());
    }
    if bot == *current_bot {
        return Ok(());
    }
    if cfg.offset_bot_id == bot {
        *offset = cfg.offset;
    } else {
        *offset = 0;
        ctx.state
            .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                // Only if the config still names this bot's token.
                if c.token.as_deref().and_then(bot_id_of) == bot {
                    c.offset = 0;
                    c.offset_bot_id = bot;
                }
                Ok(())
            })?;
    }
    *current_bot = bot;
    Ok(())
}

/// `run_once` for the bot currently saved in the config. Convenience for callers
/// that did not build `api` from a known token.
pub fn run_once(ctx: &Ctx, api: &dyn TelegramApi, offset: &mut i64) -> Result<()> {
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    let bot = cfg.token.as_deref().and_then(bot_id_of);
    run_once_for(ctx, api, bot, offset)
}

/// One `getUpdates(offset, 30)` made with the token of bot `bot`; handles each
/// update and advances `offset`. The config is re-read before every update: if the
/// saved token now belongs to another bot (reconnect during the long poll), the
/// rest of the batch is dropped unhandled and `offset` is left alone for
/// `sync_offset` to reset. A `getUpdates` error leaves `offset` unchanged.
/// Per-update handler or send failures are reported on stderr and the update is
/// still consumed, so one bad message cannot be replayed forever (and a delivered
/// message is never re-run).
pub fn run_once_for(ctx: &Ctx, api: &dyn TelegramApi, bot: Option<i64>, offset: &mut i64) -> Result<()> {
    let updates = api.get_updates(*offset, POLL_TIMEOUT_SECS)?;
    for u in updates {
        let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
        if cfg.token.as_deref().and_then(bot_id_of) != bot {
            warn(ctx, "token changed during poll, dropping fetched updates");
            return Ok(());
        }
        let receiver_before = cfg.receiver.clone();
        *offset = (*offset).max(u.update_id + 1);
        // Persist before handling: a crash mid-update must not re-run a message.
        persist_offset(ctx, bot, *offset);
        if u.callback_query.is_some() {
            match handle_callback(ctx, &u) {
                Ok(Some(r)) => {
                    if let Err(e) = send_callback_reply(api, &r) {
                        warn(ctx, &format!("could not answer update {}: {e:#}", u.update_id));
                    }
                }
                Ok(None) => {}
                Err(e) => warn(ctx, &format!("update {} failed: {e:#}", u.update_id)),
            }
            sync_pin_if_changed(ctx, api, &receiver_before);
            continue;
        }
        let is_file = u.message.as_ref().is_some_and(|m| m.text.is_none() && m.attachment().is_some());
        let handled = if is_file { handle_file(ctx, api, &u) } else { handle_update(ctx, &u) };
        match handled {
            Ok(Some(r)) => {
                if let Err(e) = send_reply(api, &r) {
                    warn(ctx, &format!("could not answer update {}: {e:#}", u.update_id));
                }
            }
            Ok(None) => {}
            Err(e) => warn(ctx, &format!("update {} failed: {e:#}", u.update_id)),
        }
        sync_pin_if_changed(ctx, api, &receiver_before);
    }
    Ok(())
}

/// Refreshes the pinned bar when the default is no longer `before`.
fn sync_pin_if_changed(ctx: &Ctx, api: &dyn TelegramApi, before: &Option<String>) {
    let now = ctx.state.read_json::<TelegramConfig>(TELEGRAM_FILE).ok().and_then(|c| c.receiver);
    if &now != before {
        sync_pin_warn(ctx, api);
    }
}

/// Records (or clears) this process as the running bot in telegram.json. Clearing
/// only touches the record if it is still ours.
fn set_bot_pid(ctx: &Ctx, on: bool) {
    let me = std::process::id();
    let now = ctx.clock.now().timestamp();
    let r = ctx
        .state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            if on {
                c.bot_pid = Some(me);
                c.bot_started_at = Some(now);
            } else if c.bot_pid == Some(me) {
                c.bot_pid = None;
                c.bot_started_at = None;
            }
            Ok(())
        });
    if let Err(e) = r {
        warn(ctx, &format!("{e:#}"));
    }
}

/// Loops `run_once` until the token is cleared (disconnect). The API is rebuilt
/// from the saved token on every pass, so a reconnect with a new token is picked up.
pub fn run_loop(
    ctx: &Ctx,
    api_for: &dyn Fn(&str) -> Box<dyn TelegramApi>,
    offset: &mut i64,
    sleep: &dyn Fn(Duration),
) {
    run_loop_with(ctx, api_for, offset, sleep, &sched::ShellRunner);
}

/// `run_loop` with the command runner for scheduled command jobs. The scheduler lives
/// only here: it ticks about every 30 s between polls, and the built-in inbox cleanup
/// runs at start and hourly.
pub fn run_loop_with(
    ctx: &Ctx,
    api_for: &dyn Fn(&str) -> Box<dyn TelegramApi>,
    offset: &mut i64,
    sleep: &dyn Fn(Duration),
    runner: &dyn sched::CmdRunner,
) {
    let started = ctx.clock.now();
    let mut last_tick: Option<DateTime<Utc>> = None;
    let mut last_clean: Option<DateTime<Utc>> = None;
    let mut current_bot: Option<i64> = None;
    let mut registered = false;
    // Token the command menu was last registered for.
    let mut menu_for: Option<String> = None;
    loop {
        let cfg: TelegramConfig = match ctx.state.read_json(TELEGRAM_FILE) {
            Ok(c) => c,
            Err(e) => {
                warn(ctx, &format!("{e:#}"));
                sleep(RETRY_SLEEP);
                continue;
            }
        };
        let Some(token) = cfg.token.clone() else {
            if registered {
                set_bot_pid(ctx, false);
            }
            return;
        };
        if !registered {
            registered = true;
            set_bot_pid(ctx, true);
        }
        if let Err(e) = sync_offset(ctx, &cfg, &token, offset, &mut current_bot) {
            warn(ctx, &format!("{e:#}"));
            sleep(RETRY_SLEEP);
            continue;
        }
        let api = api_for(&token);
        if menu_for.as_deref() != Some(token.as_str()) {
            register_commands(ctx, api.as_ref());
            if cfg.receiver.is_some() {
                sync_pin_warn(ctx, api.as_ref());
            }
            menu_for = Some(token.clone());
        }
        let now = ctx.clock.now();
        if last_clean.is_none_or(|t| now - t >= chrono::Duration::hours(1)) {
            last_clean = Some(now);
            if let Err(e) = inbox::cleanup(ctx) {
                warn(ctx, &format!("inbox cleanup failed: {e:#}"));
            }
        }
        if let Err(e) = run_once_for(ctx, api.as_ref(), current_bot, offset) {
            // `e` comes from the API layer, which scrubs the token.
            warn(ctx, &format!("{e:#}"));
            sleep(RETRY_SLEEP);
        }
        let now = ctx.clock.now();
        if last_tick.is_none_or(|t| now - t >= chrono::Duration::seconds(sched::TICK_SECS)) {
            last_tick = Some(now);
            if let Err(e) = sched::tick(ctx, api.as_ref(), runner, started) {
                warn(ctx, &format!("scheduler: {e:#}"));
            }
        }
    }
}

/// `colony telegram run`: never returns; exits 0 once the token is cleared.
pub fn run(ctx: &Ctx, api_for: &dyn Fn(&str) -> Box<dyn TelegramApi>) -> ! {
    let mut offset = 0;
    run_loop(ctx, api_for, &mut offset, &std::thread::sleep);
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AgentState, BackendKind, Kind};
    use crate::commands::testutil::Harness;
    use crate::telegram::api::{CallbackQuery, Chat, FakeTelegram, User};

    fn upd(id: i64, from: i64, chat: i64, kind: &str, text: &str) -> Update {
        Update {
            update_id: id,
            message: Some(Message {
                message_id: id,
                from: Some(User { id: from }),
                chat: Chat {
                    id: chat,
                    kind: kind.to_string(),
                },
                date: 1_000_000,
                text: Some(text.to_string()),
                reply_to_message: None,
                ..Default::default()
            }),
            callback_query: None,
        }
    }

    fn priv_msg(text: &str) -> Update {
        upd(5, 42, 42, "private", text)
    }

    fn locked(h: &Harness) {
        set_cfg(h, |c| {
            c.token = Some("111:SECRET".to_string());
            c.locked_user_id = Some(42);
            c.locked_chat_id = Some(42);
        });
    }

    fn set_cfg(h: &Harness, f: impl FnOnce(&mut TelegramConfig)) {
        h.state
            .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                f(c);
                Ok(())
            })
            .unwrap();
    }

    fn session(h: &Harness, name: &str, pane: &str) {
        h.backends.herdr.add_pane(pane, None);
        h.add_name(name, BackendKind::Herdr, pane, Kind::Claude);
    }

    fn answer(h: &Harness, u: &Update) -> String {
        handle_update(&h.ctx(), u).unwrap().unwrap().text
    }

    fn pending(h: &Harness) -> Pending {
        h.state.read_json(PENDING_FILE).unwrap()
    }

    #[test]
    fn first_private_message_locks_and_is_not_run() {
        let h = Harness::herdr();
        set_cfg(&h, |c| c.connected_at = Some(999_999));
        session(&h, "bo", "w1:p2");
        let r = handle_update(&h.ctx(), &upd(9, 42, 4242, "private", "/list"))
            .unwrap()
            .unwrap();
        assert_eq!(r.text, "\u{1f512} locked to you, send /list");
        assert_eq!(r.chat_id, 4242);
        assert_eq!(r.reply_to, 9);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, Some(42));
        assert_eq!(cfg.locked_chat_id, Some(4242));
        assert!(h.backends.herdr.log.borrow().is_empty());
        assert!(h.backends.tmux.log.borrow().is_empty());
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn other_user_ignored() {
        let h = Harness::herdr();
        locked(&h);
        assert_eq!(handle_update(&h.ctx(), &upd(1, 99, 99, "private", "/list")).unwrap(), None);
    }

    #[test]
    fn group_chat_ignored_even_for_locked_user() {
        let h = Harness::herdr();
        locked(&h);
        assert_eq!(
            handle_update(&h.ctx(), &upd(1, 42, -100, "supergroup", "/list")).unwrap(),
            None
        );
        // And a group message never locks an unlocked bot.
        let h2 = Harness::herdr();
        set_cfg(&h2, |c| c.connected_at = Some(999_999));
        assert_eq!(handle_update(&h2.ctx(), &upd(1, 42, -100, "group", "hi")).unwrap(), None);
        let cfg: TelegramConfig = h2.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, None);
    }

    #[test]
    fn no_message_or_text_is_ignored() {
        let h = Harness::herdr();
        locked(&h);
        let none = Update { update_id: 1, message: None, callback_query: None };
        assert_eq!(handle_update(&h.ctx(), &none).unwrap(), None);
        let mut notext = priv_msg("x");
        notext.message.as_mut().unwrap().text = None;
        assert_eq!(handle_update(&h.ctx(), &notext).unwrap(), None);
    }

    #[test]
    fn list_works_with_no_claude_session() {
        let h = Harness::herdr();
        locked(&h);
        h.backends.herdr.add_pane("w1:p3", None);
        h.add_name("sh", BackendKind::Herdr, "w1:p3", Kind::Shell);
        let t = answer(&h, &priv_msg("/list"));
        assert!(t.contains("sh - shell"), "{t}");
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn list_and_plain_text_include_discovered_sessions() {
        let h = Harness::herdr();
        locked(&h);
        h.backends.herdr.add_claude("w1:p6", AgentState::Idle, Some("\"solo\""));
        h.backends.herdr.add_claude("w1:p7", AgentState::Idle, Some("Fixing the thing"));
        let t = answer(&h, &priv_msg("/list"));
        assert!(t.contains("solo - claude* - idle (herdr w1:p6)"), "{t}");
        assert!(t.contains("w1:p7 - claude* - idle (herdr w1:p7)"), "{t}");
        assert!(t.ends_with("* not registered"), "{t}");
        set_cfg(&h, |c| c.receiver = Some("solo".to_string()));
        assert_eq!(answer(&h, &priv_msg("hi")), "\u{1f4e8} sent to solo");
        assert!(h.backends.herdr.sends()[0].starts_with("send w1:p6 "));
        set_cfg(&h, |c| c.receiver = Some("w1:p7".to_string()));
        assert_eq!(answer(&h, &priv_msg("yo")), "\u{1f4e8} sent to w1:p7");
        assert!(h.backends.herdr.sends()[1].starts_with("send w1:p7 "));
        assert!(h.names().0.is_empty());
    }

    #[test]
    fn plain_text_creates_pending_and_delivers_telegram_envelope() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo-server", "w1:p2");
        set_cfg(&h, |c| c.receiver = Some("bo-server".to_string()));
        let t = answer(&h, &priv_msg("status?"));
        assert_eq!(t, "\u{1f4e8} sent to bo-server");
        let p = pending(&h);
        assert_eq!(p.0.len(), 1);
        let (id, e) = p.0.iter().next().unwrap();
        assert_eq!(e.target, "bo-server");
        assert_eq!(e.chat_id, 42);
        assert_eq!(e.message_id, 5);
        let sends = h.backends.herdr.sends();
        assert_eq!(sends.len(), 1);
        assert!(sends[0].starts_with("send w1:p2 "), "{sends:?}");
        assert!(sends[0].contains(&format!("reply-id {id}")), "{sends:?}");
        assert!(sends[0].contains("Message: status?"), "{sends:?}");
    }

    #[test]
    fn failed_delivery_removes_pending() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        h.backends.herdr.fail_send.set(true);
        let t = answer(&h, &priv_msg("hi"));
        assert!(t.starts_with("\u{26a0}\u{fe0f} could not send to bo"), "{t}");
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn removed_commands_answer_with_the_hint() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        for cmd in ["/send bo hi", "/last bo 5", "/help", "/schedule", "/schedule add x 0 9 * * * bo hi", "/start", "/foo bar", "/Settings", "/"] {
            assert_eq!(answer(&h, &priv_msg(cmd)), ONLY_HINT, "{cmd}");
        }
        assert_eq!(ONLY_HINT, "\u{2139}\u{fe0f} only /list and /use \u{2014} type normally to talk to your session");
        assert!(h.backends.herdr.sends().is_empty());
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn command_with_bot_suffix() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        let t = answer(&h, &priv_msg("/list@colony_bot"));
        assert!(t.contains("bo - claude"), "{t}");
    }

    #[test]
    fn plain_text_to_receiver() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "leader", "w1:p2");
        set_cfg(&h, |c| c.receiver = Some("leader".to_string()));
        assert_eq!(answer(&h, &priv_msg("how is it going")), "\u{1f4e8} sent to leader");
        let sends = h.backends.herdr.sends();
        assert!(sends[0].contains("Message: how is it going"), "{sends:?}");
        assert_eq!(pending(&h).0.len(), 1);
    }

    #[test]
    fn plain_text_dead_receiver_replies_error_no_pending() {
        let h = Harness::herdr();
        locked(&h);
        set_cfg(&h, |c| c.receiver = Some("gone".to_string()));
        assert_eq!(answer(&h, &priv_msg("hi")), "\u{26a0}\u{fe0f} receiver gone not found, use /use");
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn plain_text_no_receiver() {
        let h = Harness::herdr();
        locked(&h);
        assert_eq!(
            answer(&h, &priv_msg("hi")),
            "\u{2753} no session selected, use /use <name>"
        );
    }

    #[test]
    fn run_once_advances_offset() {
        let h = Harness::herdr();
        locked(&h);
        let api = FakeTelegram::new();
        api.updates.borrow_mut().push_back(vec![priv_msg("/list")]);
        let mut offset = 0;
        run_once(&h.ctx(), &api, &mut offset).unwrap();
        assert_eq!(offset, 6);
        assert_eq!(api.offsets.borrow()[0], 0);
        let sent = api.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].0, sent[0].2), (42, Some(5)));
    }

    #[test]
    fn run_once_sends_split_reply() {
        let h = Harness::herdr();
        locked(&h);
        for i in 0..300 {
            session(&h, &format!("session-{i:03}"), &format!("w1:p{}", 200 + i));
        }
        let api = FakeTelegram::new();
        api.updates.borrow_mut().push_back(vec![priv_msg("/list")]);
        let mut offset = 0;
        run_once(&h.ctx(), &api, &mut offset).unwrap();
        let sent = api.sent.borrow();
        assert!(sent.len() >= 2, "{}", sent.len());
        assert_eq!(sent[0].2, Some(5));
        assert!(sent[1..].iter().all(|s| s.2.is_none()));
        assert!(sent.iter().all(|s| s.1.encode_utf16().count() <= TG_LIMIT));
    }

    #[test]
    fn send_reply_splits_10000_into_3() {
        let api = FakeTelegram::new();
        let r = BotReply {
            chat_id: 42,
            reply_to: 5,
            text: "a".repeat(10_000),
            markup: None,
        };
        send_reply(&api, &r).unwrap();
        let sent = api.sent.borrow();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0].2, Some(5));
        assert_eq!(sent[1].2, None);
        assert_eq!(sent[2].2, None);
    }

    #[test]
    fn run_once_error_does_not_advance_offset() {
        let h = Harness::herdr();
        locked(&h);
        let api = FakeTelegram::new();
        api.fail_updates.set(true);
        let mut offset = 7;
        assert!(run_once(&h.ctx(), &api, &mut offset).is_err());
        assert_eq!(offset, 7);
    }

    #[test]
    fn run_exits_when_token_cleared() {
        let h = Harness::herdr();
        // No token saved: the loop returns at once and never polls.
        let polled = std::cell::Cell::new(false);
        let f = |_t: &str| -> Box<dyn TelegramApi> {
            polled.set(true);
            Box::new(FakeTelegram::new())
        };
        let mut offset = 0;
        run_loop(&h.ctx(), &f, &mut offset, &|_| {});
        assert!(!polled.get());
    }

    #[test]
    fn run_loop_records_pid_then_clears_it_on_exit() {
        let h = Harness::herdr();
        locked(&h);
        let seen = std::cell::Cell::new(None);
        let state = &h.state;
        let f = |_t: &str| -> Box<dyn TelegramApi> {
            let c: TelegramConfig = state.read_json(TELEGRAM_FILE).unwrap();
            seen.set(Some((c.bot_pid, c.bot_started_at)));
            state
                .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                    c.token = None;
                    Ok(())
                })
                .unwrap();
            Box::new(FakeTelegram::new())
        };
        let mut offset = 0;
        run_loop(&h.ctx(), &f, &mut offset, &|_| {});
        let (pid, started) = seen.get().unwrap();
        assert_eq!(pid, Some(std::process::id()));
        assert_eq!(started, Some(h.ctx().clock.now().timestamp()));
        let c: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!((c.bot_pid, c.bot_started_at), (None, None));
        let mode = std::fs::metadata(h.state.path(TELEGRAM_FILE)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn run_loop_polls_then_stops_when_token_cleared() {
        let h = Harness::herdr();
        locked(&h);
        let calls = std::cell::Cell::new(0);
        let state = &h.state;
        let f = |_t: &str| -> Box<dyn TelegramApi> {
            calls.set(calls.get() + 1);
            // Disconnect while the first poll is in flight.
            state
                .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                    c.token = None;
                    Ok(())
                })
                .unwrap();
            Box::new(FakeTelegram::new())
        };
        let mut offset = 0;
        run_loop(&h.ctx(), &f, &mut offset, &|_| {});
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn pre_connect_message_cannot_lock_and_is_not_executed() {
        let h = Harness::herdr();
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| c.connected_at = Some(2_000_000));
        let mut stale = upd(1, 66, 66, "private", "rm -rf");
        stale.message.as_mut().unwrap().date = 1_999_999;
        assert_eq!(handle_update(&h.ctx(), &stale).unwrap(), None);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, None);
        assert!(h.backends.herdr.sends().is_empty());
        assert!(pending(&h).0.is_empty());
        // A message after the connect second claims the lock.
        let mut fresh = upd(2, 77, 77, "private", "hi");
        fresh.message.as_mut().unwrap().date = 2_000_001;
        assert_eq!(answer(&h, &fresh), "\u{1f512} locked to you, send /list");
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, Some(77));
        // After the lock, a stale message from the locked user is still not run.
        let mut old = upd(3, 77, 77, "private", "x");
        old.message.as_mut().unwrap().date = 5;
        assert_eq!(handle_update(&h.ctx(), &old).unwrap(), None);
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn unlocked_without_connected_at_cannot_lock() {
        let h = Harness::herdr();
        assert_eq!(handle_update(&h.ctx(), &priv_msg("hi")).unwrap(), None);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, None);
    }

    #[test]
    fn offset_persisted_and_reloaded_so_update_not_handled_twice() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| {
            c.offset_bot_id = Some(111);
            c.receiver = Some("bo".to_string());
        });
        let api = FakeTelegram::new();
        api.updates.borrow_mut().push_back(vec![priv_msg("once")]);
        let mut offset = 0;
        run_once(&h.ctx(), &api, &mut offset).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.offset, 6);
        assert_eq!(cfg.offset_bot_id, Some(111));
        // "Restart": a fresh in-memory offset is loaded from state by run_loop.
        let api2 = FakeTelegram::new();
        let mut fresh = 0;
        let state = &h.state;
        let f = |_t: &str| -> Box<dyn TelegramApi> {
            state
                .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                    c.token = None;
                    Ok(())
                })
                .unwrap();
            Box::new(FakeTelegram::new())
        };
        run_loop(&h.ctx(), &f, &mut fresh, &|_| {});
        assert_eq!(fresh, 6);
        run_once(&h.ctx(), &api2, &mut fresh).unwrap();
        assert_eq!(api2.offsets.borrow()[0], 6);
        assert_eq!(h.backends.herdr.sends().len(), 1);
    }

    #[test]
    fn offset_reset_on_bot_id_change() {
        let h = Harness::herdr();
        locked(&h); // token 111:SECRET
        set_cfg(&h, |c| {
            c.offset = 50;
            c.offset_bot_id = Some(999);
        });
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        let mut offset = 50;
        let mut cur = None;
        sync_offset(&h.ctx(), &cfg, "111:SECRET", &mut offset, &mut cur).unwrap();
        assert_eq!(offset, 0);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!((cfg.offset, cfg.offset_bot_id), (0, Some(111)));
        // Same bot: the saved offset is kept and loaded.
        set_cfg(&h, |c| c.offset = 8);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        let mut cur = None;
        sync_offset(&h.ctx(), &cfg, "111:SECRET", &mut offset, &mut cur).unwrap();
        assert_eq!(offset, 8);
    }

    fn reconnect(h: &Harness, token: &str) {
        // What `connect` writes for a different bot.
        set_cfg(h, |c| {
            c.token = Some(token.to_string());
            c.locked_user_id = None;
            c.locked_chat_id = None;
            c.offset = 0;
            c.offset_bot_id = bot_id_of(token);
            c.connected_at = Some(1_000_000);
        });
    }

    #[test]
    fn batch_fetched_under_old_token_is_dropped_after_reconnect() {
        let h = Harness::herdr();
        locked(&h); // bot 111
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| c.offset_bot_id = Some(111));
        // The long poll for bot 111 returns a stranger's message and an owner command,
        // but `connect <222:NEW>` happened while it was in flight.
        struct ReconnectingApi<'a>(&'a Harness, FakeTelegram);
        impl TelegramApi for ReconnectingApi<'_> {
            fn get_me(&self) -> Result<super::super::api::BotInfo> {
                self.1.get_me()
            }
            fn get_updates(&self, offset: i64, timeout: u64) -> Result<Vec<Update>> {
                let r = self.1.get_updates(offset, timeout);
                reconnect(self.0, "222:NEW");
                r
            }
            fn send_message(&self, chat: i64, text: &str, reply_to: Option<i64>) -> Result<i64> {
                self.1.send_message(chat, text, reply_to)
            }
            fn set_my_commands(&self, c: &[(&str, &str)]) -> Result<()> {
                self.1.set_my_commands(c)
            }
        }
        let fake = FakeTelegram::new();
        let mut stranger = upd(900, 66, 66, "private", "hello");
        stranger.message.as_mut().unwrap().date = 2_000_000;
        let mut owner = upd(901, 42, 42, "private", "x");
        owner.message.as_mut().unwrap().date = 2_000_000;
        fake.updates.borrow_mut().push_back(vec![stranger, owner]);
        let api = ReconnectingApi(&h, fake);
        let mut offset = 0;
        run_once_for(&h.ctx(), &api, Some(111), &mut offset).unwrap();
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, None, "stranger must not claim the new bot's lock");
        assert_eq!(cfg.offset, 0);
        assert_eq!(cfg.offset_bot_id, Some(222));
        assert!(api.1.sent.borrow().is_empty());
        assert!(h.backends.herdr.sends().is_empty());
        assert_eq!(offset, 0);
    }

    #[test]
    fn offset_resets_for_new_bot_id_and_old_offset_is_not_persisted() {
        let h = Harness::herdr();
        locked(&h);
        set_cfg(&h, |c| {
            c.offset = 500;
            c.offset_bot_id = Some(111);
        });
        // Loop is synced with bot 111 at a high offset.
        let mut offset = 500;
        let mut cur = Some(111);
        // Reconnect to bot 222 (connect already reset the saved offset to 0).
        reconnect(&h, "222:NEW");
        // A late persist by the old bot's loop must not touch the new config.
        persist_offset(&h.ctx(), Some(111), 501);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!((cfg.offset, cfg.offset_bot_id), (0, Some(222)));
        // Loop notices the new bot id: in-memory offset follows the saved one.
        sync_offset(&h.ctx(), &cfg, "222:NEW", &mut offset, &mut cur).unwrap();
        assert_eq!((offset, cur), (0, Some(222)));
        // With a saved offset for the new bot, that one is loaded.
        set_cfg(&h, |c| c.offset = 12);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        let mut off2 = 500;
        let mut cur2 = Some(111);
        sync_offset(&h.ctx(), &cfg, "222:NEW", &mut off2, &mut cur2).unwrap();
        assert_eq!(off2, 12);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.offset, 12);
    }

    #[test]
    fn message_dated_exactly_connected_at_cannot_lock_or_run() {
        let h = Harness::herdr();
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| c.connected_at = Some(2_000_000));
        let mut same = upd(1, 66, 66, "private", "hi");
        same.message.as_mut().unwrap().date = 2_000_000;
        assert_eq!(handle_update(&h.ctx(), &same).unwrap(), None);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, None);
        // Same for an already locked bot: not executed.
        set_cfg(&h, |c| c.locked_user_id = Some(66));
        let mut cmd = upd(2, 66, 66, "private", "x");
        cmd.message.as_mut().unwrap().date = 2_000_000;
        assert_eq!(handle_update(&h.ctx(), &cmd).unwrap(), None);
        assert!(h.backends.herdr.sends().is_empty());
    }

    fn receiver_of(h: &Harness) -> Option<String> {
        let c: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        c.receiver
    }

    #[test]
    fn use_sets_shows_and_clears() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        assert_eq!(answer(&h, &priv_msg("/use BO")), "\u{1f3af} default session: bo");
        assert_eq!(receiver_of(&h).as_deref(), Some("bo"));
        assert_eq!(answer(&h, &priv_msg("/use off")), "\u{1f3af} default session cleared");
        assert_eq!(receiver_of(&h), None);
    }

    fn live(h: &Harness, name: &str, pane: &str, st: AgentState) {
        h.backends.herdr.add_pane(pane, Some(("claude", st)));
        h.add_name(name, BackendKind::Herdr, pane, Kind::Claude);
    }

    fn buttons(markup: &Value) -> Vec<(String, String)> {
        markup["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let b = &row.as_array().unwrap()[0];
                (b["text"].as_str().unwrap().to_string(), b["callback_data"].as_str().unwrap().to_string())
            })
            .collect()
    }

    fn tap(id: i64, from: i64, kind: &str, data: &str) -> Update {
        Update {
            update_id: id,
            message: None,
            callback_query: Some(CallbackQuery {
                id: format!("cb{id}"),
                from: User { id: from },
                message: Some(Message {
                    message_id: 77,
                    from: None,
                    chat: Chat { id: from, kind: kind.to_string() },
                    date: 1_000_000,
                    text: Some(USE_PROMPT.to_string()),
                    reply_to_message: None,
                    ..Default::default()
                }),
                data: Some(data.to_string()),
            }),
        }
    }

    #[test]
    fn use_alone_sends_keyboard_with_marker() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        live(&h, "ca", "w1:p3", AgentState::Working);
        h.backends.herdr.add_claude("w1:p6", AgentState::Blocked, Some("\"solo\""));
        let r = handle_update(&h.ctx(), &priv_msg("/use")).unwrap().unwrap();
        assert_eq!(r.text, "\u{1f3af} Choose default session:");
        let b = buttons(r.markup.as_ref().unwrap());
        assert_eq!(b.len(), 3, "{b:?}");
        assert_eq!(b[0], ("\u{1f7e2} bo".to_string(), "use:w1:p2".to_string()));
        assert_eq!(b[1], ("\u{1f7e1} ca".to_string(), "use:w1:p3".to_string()));
        assert_eq!(b[2], ("\u{1f534} solo".to_string(), "use:w1:p6".to_string()));
        assert!(b.iter().all(|(_, d)| d.len() <= 64));
        // The default carries the check.
        answer(&h, &priv_msg("/use ca"));
        let r = handle_update(&h.ctx(), &priv_msg("/use")).unwrap().unwrap();
        let b = buttons(r.markup.as_ref().unwrap());
        assert_eq!(b[1].0, "\u{2705} \u{1f7e1} ca");
        assert_eq!(b[0].0, "\u{1f7e2} bo");
        // And it goes out as a keyboard message.
        let api = FakeTelegram::new();
        send_reply(&api, &r).unwrap();
        assert_eq!(api.keyboards.borrow().len(), 1);
        assert_eq!(api.keyboards.borrow()[0].1, "\u{1f3af} Choose default session:");
    }

    #[test]
    fn callback_sets_default_and_moves_marker() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        live(&h, "ca", "w1:p3", AgentState::Idle);
        let api = FakeTelegram::new();
        let r = handle_callback(&h.ctx(), &tap(1, 42, "private", "use:w1:p3")).unwrap().unwrap();
        send_callback_reply(&api, &r).unwrap();
        assert_eq!(receiver_of(&h).as_deref(), Some("ca"));
        assert_eq!(api.answers.borrow().as_slice(), &[("cb1".to_string(), "\u{1f3af} ca".to_string())]);
        // The keyboard is gone: plain text edit, no markup.
        assert!(api.edits.borrow().is_empty());
        assert_eq!(api.text_edits.borrow().as_slice(), &[(42, 77, "\u{1f3af} Default session: ca".to_string())]);
        // off clears it and the check goes away.
        let r = handle_callback(&h.ctx(), &tap(2, 42, "private", "use:off")).unwrap().unwrap();
        assert_eq!(receiver_of(&h), None);
        assert_eq!(r.toast, "\u{1f3af} default cleared");
        let e = r.edit.unwrap();
        assert_eq!(e.2, "\u{1f3af} Default session cleared");
        assert!(e.3.is_none());
    }

    #[test]
    fn callback_from_other_user_or_group_ignored() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        assert_eq!(handle_callback(&h.ctx(), &tap(1, 99, "private", "use:w1:p2")).unwrap(), None);
        assert_eq!(handle_callback(&h.ctx(), &tap(2, 42, "supergroup", "use:w1:p2")).unwrap(), None);
        assert_eq!(receiver_of(&h), None);
        // Through the loop nothing is answered either.
        let api = FakeTelegram::new();
        api.updates.borrow_mut().push_back(vec![tap(3, 99, "private", "use:w1:p2")]);
        let mut offset = 0;
        run_once(&h.ctx(), &api, &mut offset).unwrap();
        assert!(api.answers.borrow().is_empty() && api.edits.borrow().is_empty());
        assert_eq!(receiver_of(&h), None);
        // An unlocked bot is not claimed by a tap.
        let h2 = Harness::herdr();
        set_cfg(&h2, |c| c.connected_at = Some(1));
        assert_eq!(handle_callback(&h2.ctx(), &tap(4, 42, "private", "use:off")).unwrap(), None);
    }

    #[test]
    fn callback_for_vanished_session_toasts_and_refreshes() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        let r = handle_callback(&h.ctx(), &tap(1, 42, "private", "use:w1:p9")).unwrap().unwrap();
        assert_eq!(r.toast, "\u{26a0}\u{fe0f} session gone");
        assert_eq!(receiver_of(&h), None);
        let b = buttons(&r.edit.unwrap().3.unwrap());
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].1, "use:w1:p2");
    }

    #[test]
    fn callback_data_over_64_bytes_uses_index() {
        let h = Harness::herdr();
        locked(&h);
        let long = format!("w1:{}", "p".repeat(80));
        live(&h, "bo", &long, AgentState::Idle);
        let r = handle_update(&h.ctx(), &priv_msg("/use")).unwrap().unwrap();
        let b = buttons(r.markup.as_ref().unwrap());
        assert_eq!(b[0].1, "use:#0");
        handle_callback(&h.ctx(), &tap(1, 42, "private", "use:#0")).unwrap().unwrap();
        assert_eq!(receiver_of(&h).as_deref(), Some("bo"));
    }

    fn record_sent(h: &Harness, mid: i64, target: &str) {
        h.state
            .update_json(SENT_FILE, crate::state::sent::SENT_MODE, |s: &mut Sent| {
                s.record(h.ctx().clock.now(), 42, &[mid], target);
                Ok(())
            })
            .unwrap();
    }

    fn swipe(id: i64, reply_to: i64, text: &str) -> Update {
        let mut u = priv_msg(text);
        u.update_id = id;
        let m = u.message.as_mut().unwrap();
        m.message_id = id;
        m.reply_to_message = Some(crate::telegram::api::ReplyRef { message_id: reply_to });
        u
    }

    #[test]
    fn swipe_reply_routes_to_recorded_sender_without_changing_default() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        live(&h, "ca", "w1:p3", AgentState::Idle);
        answer(&h, &priv_msg("/use ca"));
        record_sent(&h, 500, "bo");
        let t = answer(&h, &swipe(9, 500, "and the tests?"));
        assert_eq!(t, "\u{1f4e8} sent to bo");
        let sends = h.backends.herdr.sends();
        assert!(sends[0].starts_with("send w1:p2 "), "{sends:?}");
        assert!(sends[0].contains("Message: and the tests?"), "{sends:?}");
        assert_eq!(receiver_of(&h).as_deref(), Some("ca"));
        let p = pending(&h);
        assert_eq!(p.0.len(), 1);
        assert_eq!(p.0.values().next().unwrap().target, "bo");
    }

    #[test]
    fn swipe_reply_to_busy_sender_says_busy() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Working);
        record_sent(&h, 500, "bo");
        assert!(answer(&h, &swipe(10, 500, "x")).contains("busy"));
    }

    #[test]
    fn swipe_reply_to_unknown_message_falls_back_to_default() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        live(&h, "ca", "w1:p3", AgentState::Idle);
        answer(&h, &priv_msg("/use ca"));
        record_sent(&h, 500, "bo");
        assert_eq!(answer(&h, &swipe(9, 123, "hi")), "\u{1f4e8} sent to ca");
        assert!(h.backends.herdr.sends()[0].starts_with("send w1:p3 "));
        // Without a default, an unknown reply gets the usual hint.
        answer(&h, &priv_msg("/use off"));
        assert!(answer(&h, &swipe(10, 123, "hi")).starts_with("\u{2753} no session selected"));
    }

    #[test]
    fn swipe_reply_to_gone_sender_is_an_error_not_misrouted() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "ca", "w1:p3", AgentState::Idle);
        answer(&h, &priv_msg("/use ca"));
        record_sent(&h, 500, "bo");
        assert_eq!(answer(&h, &swipe(9, 500, "hi")), "\u{26a0}\u{fe0f} no session named bo");
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn use_unknown_changes_nothing() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        answer(&h, &priv_msg("/use bo"));
        let t = answer(&h, &priv_msg("/use ghost"));
        assert!(t.contains("ghost"), "{t}");
        assert_eq!(receiver_of(&h).as_deref(), Some("bo"));
        // A registered name whose pane is gone is refused too.
        h.add_name("dead", BackendKind::Herdr, "w1:p77", Kind::Claude);
        assert_eq!(answer(&h, &priv_msg("/use dead")), "\u{26a0}\u{fe0f} no session named dead");
        assert_eq!(receiver_of(&h).as_deref(), Some("bo"));
    }

    #[test]
    fn use_ambiguous_discovered_name_changes_nothing() {
        let h = Harness::herdr();
        locked(&h);
        h.backends.herdr.add_claude("w1:p6", AgentState::Idle, Some("\"twin\""));
        h.backends.herdr.add_claude("w1:p7", AgentState::Idle, Some("\"twin\""));
        let t = answer(&h, &priv_msg("/use twin"));
        assert!(t.contains("matches 2"), "{t}");
        assert_eq!(receiver_of(&h), None);
    }

    #[test]
    fn use_accepts_discovered_name_and_pane_id() {
        let h = Harness::herdr();
        locked(&h);
        h.backends.herdr.add_claude("w1:p6", AgentState::Idle, Some("\"solo\""));
        assert_eq!(answer(&h, &priv_msg("/use solo")), "\u{1f3af} default session: solo");
        assert_eq!(answer(&h, &priv_msg("hello")), "\u{1f4e8} sent to solo");
        assert!(h.backends.herdr.sends()[0].starts_with("send w1:p6 "));
        assert_eq!(answer(&h, &priv_msg("/use w1:p6")), "\u{1f3af} default session: w1:p6");
        assert_eq!(answer(&h, &priv_msg("again")), "\u{1f4e8} sent to w1:p6");
    }

    #[test]
    fn use_and_cli_receiver_are_one_setting() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        session(&h, "al", "w1:p3");
        answer(&h, &priv_msg("/use bo"));
        crate::telegram::cmds::receiver(&h.ctx(), Some("al"), false).unwrap();
        assert_eq!(receiver_of(&h).as_deref(), Some("al"));
        answer(&h, &priv_msg("/use bo"));
        assert_eq!(receiver_of(&h).as_deref(), Some("bo"));
    }

    #[test]
    fn plain_text_follows_use_target() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        session(&h, "al", "w1:p3");
        answer(&h, &priv_msg("/use al"));
        assert_eq!(answer(&h, &priv_msg("status?")), "\u{1f4e8} sent to al");
        assert!(h.backends.herdr.sends()[0].starts_with("send w1:p3 "));
        answer(&h, &priv_msg("/use bo"));
        assert_eq!(answer(&h, &priv_msg("and you?")), "\u{1f4e8} sent to bo");
        assert!(h.backends.herdr.sends()[1].starts_with("send w1:p2 "));
        answer(&h, &priv_msg("/use off"));
        assert_eq!(
            answer(&h, &priv_msg("hi")),
            "\u{2753} no session selected, use /use <name>"
        );
        assert_eq!(h.backends.herdr.sends().len(), 2);
    }

    fn working(h: &Harness, pane: &str, st: AgentState) {
        h.backends.herdr.add_pane(pane, Some(("claude", st)));
    }

    #[test]
    fn busy_session_ack_says_so() {
        let h = Harness::herdr();
        locked(&h);
        working(&h, "w1:p2", AgentState::Working);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        let want = "\u{1f4e8} sent to bo (\u{23f3} busy \u{2014} it will answer after its current task)";
        answer(&h, &priv_msg("/use bo"));
        assert_eq!(answer(&h, &priv_msg("plain")), want);
        assert_eq!(h.backends.herdr.sends().len(), 1);
        assert_eq!(pending(&h).0.len(), 1);
    }

    #[test]
    fn idle_and_tmux_ack_is_plain() {
        let h = Harness::herdr();
        locked(&h);
        working(&h, "w1:p2", AgentState::Idle);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        answer(&h, &priv_msg("/use bo"));
        assert_eq!(answer(&h, &priv_msg("hi")), "\u{1f4e8} sent to bo");
        h.backends.tmux.add_pane("%5", None);
        h.add_name("tw", BackendKind::Tmux, "%5", Kind::Claude);
        answer(&h, &priv_msg("/use tw"));
        assert_eq!(answer(&h, &priv_msg("hi")), "\u{1f4e8} sent to tw");
    }

    #[test]
    fn blocked_session_refused_without_pending() {
        let h = Harness::herdr();
        locked(&h);
        working(&h, "w1:p2", AgentState::Blocked);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        answer(&h, &priv_msg("/use bo"));
        assert_eq!(
            answer(&h, &priv_msg("plain")),
            "\u{26d4} not delivered: bo is waiting on a prompt"
        );
        assert!(h.backends.herdr.sends().is_empty());
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn commands_registered_at_loop_start() {
        use std::rc::Rc;
        struct Shared(Rc<FakeTelegram>);
        impl TelegramApi for Shared {
            fn get_me(&self) -> Result<super::super::api::BotInfo> {
                self.0.get_me()
            }
            fn get_updates(&self, o: i64, t: u64) -> Result<Vec<Update>> {
                self.0.get_updates(o, t)
            }
            fn send_message(&self, c: i64, t: &str, r: Option<i64>) -> Result<i64> {
                self.0.send_message(c, t, r)
            }
            fn set_my_commands(&self, c: &[(&str, &str)]) -> Result<()> {
                self.0.set_my_commands(c)
            }
        }
        for fail in [false, true] {
            let h = Harness::herdr();
            locked(&h);
            let fake = Rc::new(FakeTelegram::new());
            fake.fail_commands.set(fail);
            let state = &h.state;
            let polls = std::cell::Cell::new(0);
            let f = |_t: &str| -> Box<dyn TelegramApi> {
                polls.set(polls.get() + 1);
                if polls.get() == 2 {
                    state
                        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                            c.token = None;
                            Ok(())
                        })
                        .unwrap();
                }
                Box::new(Shared(fake.clone()))
            };
            let mut offset = 0;
            run_loop(&h.ctx(), &f, &mut offset, &|_| {});
            // The loop kept polling after the (failed) registration.
            assert_eq!(polls.get(), 2, "fail={fail}");
            assert_eq!(fake.offsets.borrow().len(), 2, "fail={fail}");
            if fail {
                assert!(fake.commands.borrow().is_empty());
            } else {
                // Once per start, not once per poll.
                let calls = fake.commands.borrow();
                assert_eq!(calls.len(), 1);
                let names: Vec<&str> = calls[0].iter().map(|(c, _)| c.as_str()).collect();
                assert_eq!(names, vec!["list", "use"]);
                assert!(calls[0][1].1.starts_with("Set default session"));
            }
        }
    }

    fn pinned_id(h: &Harness) -> Option<i64> {
        h.state.read_json::<TelegramConfig>(TELEGRAM_FILE).unwrap().pinned_message_id
    }

    fn run_updates(h: &Harness, api: &FakeTelegram, ups: Vec<Update>) {
        api.updates.borrow_mut().push_back(ups);
        let mut off = 0;
        run_once(&h.ctx(), api, &mut off).unwrap();
    }

    const BAR_BO: &str = "\u{1f4cc} CURRENT SESSION: BO";

    #[test]
    fn tap_removes_keyboard() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        let api = FakeTelegram::new();
        run_updates(&h, &api, vec![tap(1, 42, "private", "use:w1:p2")]);
        assert!(api.edits.borrow().is_empty());
        assert_eq!(api.answers.borrow().len(), 1);
        assert_eq!(api.text_edits.borrow()[0].2, "\u{1f3af} Default session: bo");
    }

    #[test]
    fn change_unpins_deletes_old_then_sends_and_pins_new() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        live(&h, "ca", "w1:p3", AgentState::Idle);
        let api = FakeTelegram::new();
        run_updates(&h, &api, vec![upd(1, 42, 42, "private", "/use bo")]);
        let first = pinned_id(&h).unwrap();
        assert!(api.ids.borrow().iter().any(|(id, c, t)| *id == first && *c == 42 && t == BAR_BO));
        assert_eq!(api.pins.borrow().as_slice(), &[(42, first)]);
        assert!(api.unpins.borrow().is_empty() && api.deletes.borrow().is_empty());
        run_updates(&h, &api, vec![upd(2, 42, 42, "private", "/use ca")]);
        let second = pinned_id(&h).unwrap();
        assert_ne!(second, first);
        assert_eq!(api.unpins.borrow().as_slice(), &[(42, first)]);
        assert_eq!(api.deletes.borrow().as_slice(), &[(42, first)]);
        assert_eq!(api.pins.borrow().as_slice(), &[(42, first), (42, second)]);
        assert!(api.ids.borrow().iter().any(|(id, _, t)| *id == second && t == "\u{1f4cc} CURRENT SESSION: CA"));
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.pinned_text.as_deref(), Some("\u{1f4cc} CURRENT SESSION: CA"));
        run_updates(&h, &api, vec![upd(3, 42, 42, "private", "/use off")]);
        let third = pinned_id(&h).unwrap();
        assert_eq!(api.deletes.borrow().last(), Some(&(42, second)));
        assert!(api.ids.borrow().iter().any(|(id, _, t)| *id == third && t == "\u{1f4cc} CURRENT SESSION: none"));
    }

    #[test]
    fn delete_failure_is_tolerated() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        let api = FakeTelegram::new();
        run_updates(&h, &api, vec![upd(1, 42, 42, "private", "/use bo")]);
        let old = pinned_id(&h).unwrap();
        api.fail_delete.set(true);
        run_updates(&h, &api, vec![upd(2, 42, 42, "private", "/use off")]);
        let new = pinned_id(&h).unwrap();
        assert_ne!(new, old);
        assert_eq!(api.pins.borrow().as_slice(), &[(42, old), (42, new)]);
        assert_eq!(receiver_of(&h), None);
    }

    #[test]
    fn unchanged_text_makes_no_calls() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        let api = FakeTelegram::new();
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        sync_pin(&h.ctx(), &api).unwrap();
        let id = pinned_id(&h).unwrap();
        let sent = api.sent.borrow().len();
        sync_pin(&h.ctx(), &api).unwrap();
        assert_eq!(pinned_id(&h), Some(id));
        assert_eq!(api.sent.borrow().len(), sent);
        assert_eq!(api.pins.borrow().len(), 1);
        assert!(api.unpins.borrow().is_empty() && api.deletes.borrow().is_empty());
        assert!(api.text_edits.borrow().is_empty());
    }

    #[test]
    fn pin_failure_does_not_block_the_change() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Idle);
        let api = FakeTelegram::new();
        api.fail_pin.set(true);
        run_updates(&h, &api, vec![upd(1, 42, 42, "private", "/use bo")]);
        assert_eq!(receiver_of(&h).as_deref(), Some("bo"));
        assert!(api.sent.borrow().iter().any(|s| s.1.contains("default session: bo")));
        assert!(api.pins.borrow().is_empty());
    }

    #[test]
    fn loop_start_refreshes_bar_when_default_set() {
        use std::rc::Rc;
        struct Shared(Rc<FakeTelegram>);
        impl TelegramApi for Shared {
            fn get_me(&self) -> Result<super::super::api::BotInfo> {
                self.0.get_me()
            }
            fn get_updates(&self, o: i64, t: u64) -> Result<Vec<Update>> {
                self.0.get_updates(o, t)
            }
            fn send_message(&self, c: i64, t: &str, r: Option<i64>) -> Result<i64> {
                self.0.send_message(c, t, r)
            }
            fn pin_message(&self, c: i64, m: i64) -> Result<()> {
                self.0.pin_message(c, m)
            }
            fn unpin_message(&self, c: i64, m: i64) -> Result<()> {
                self.0.unpin_message(c, m)
            }
            fn delete_message(&self, c: i64, m: i64) -> Result<()> {
                self.0.delete_message(c, m)
            }
            fn set_my_commands(&self, c: &[(&str, &str)]) -> Result<()> {
                self.0.set_my_commands(c)
            }
        }
        let h = Harness::herdr();
        locked(&h);
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        let fake = Rc::new(FakeTelegram::new());
        let state = &h.state;
        let polls = std::cell::Cell::new(0);
        let f = |_t: &str| -> Box<dyn TelegramApi> {
            polls.set(polls.get() + 1);
            if polls.get() == 2 {
                state
                    .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                        c.token = None;
                        Ok(())
                    })
                    .unwrap();
            }
            Box::new(Shared(fake.clone()))
        };
        let mut offset = 0;
        run_loop(&h.ctx(), &f, &mut offset, &|_| {});
        assert_eq!(fake.pins.borrow().len(), 1);
        assert!(fake.ids.borrow().iter().any(|(_, _, t)| t == BAR_BO));
    }
    // ---- photos and files from the phone ----

    use crate::telegram::api::FileRef;

    fn file_msg(id: i64, f: impl FnOnce(&mut Message)) -> Update {
        let mut u = upd(id, 42, 42, "private", "");
        let m = u.message.as_mut().unwrap();
        m.text = None;
        f(m);
        u
    }

    fn fref(id: &str) -> FileRef {
        FileRef { file_id: id.to_string(), ..Default::default() }
    }

    fn inbox_files(h: &Harness) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(h.state.path("inbox"))
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn photo_is_downloaded_and_sent_with_attachment_line() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        let api = FakeTelegram::new();
        api.add_download("big", "photos/file_2.jpg", b"BIGJPEG");
        let u = file_msg(8, |m| {
            m.photo = vec![
                FileRef { file_id: "small".into(), file_size: Some(10), ..Default::default() },
                FileRef { file_id: "big".into(), file_size: Some(7), ..Default::default() },
            ];
            m.photo[0].file_size = Some(3);
            m.caption = Some("what is this?".to_string());
        });
        let r = handle_file(&h.ctx(), &api, &u).unwrap().unwrap();
        assert_eq!(r.text, "\u{1f4ce} sent to bo");
        let files = inbox_files(&h);
        assert_eq!(files.len(), 1);
        assert_eq!(std::fs::read(&files[0]).unwrap(), b"BIGJPEG");
        // the largest size was requested
        assert_eq!(*api.file_requests.borrow(), vec!["small".to_string()].into_iter().filter(|_| false).chain(["big".to_string()]).collect::<Vec<_>>());
        let sends = h.backends.herdr.sends();
        assert_eq!(sends.len(), 1);
        assert!(sends[0].contains("From: telegram (reply-id "), "{sends:?}");
        assert!(
            sends[0].contains(&format!("Message: what is this?\nAttachment: {}\n", files[0].display())),
            "{sends:?}"
        );
        assert_eq!(pending(&h).0.len(), 1);
    }

    #[test]
    fn document_without_caption_and_busy_rule() {
        let h = Harness::herdr();
        locked(&h);
        live(&h, "bo", "w1:p2", AgentState::Working);
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        let api = FakeTelegram::new();
        api.add_download("d1", "documents/file_5.pdf", b"%PDF");
        let u = file_msg(9, |m| m.document = Some(fref("d1")));
        let r = handle_file(&h.ctx(), &api, &u).unwrap().unwrap();
        assert!(r.text.starts_with("\u{1f4ce} sent to bo (\u{23f3} busy"), "{}", r.text);
        let files = inbox_files(&h);
        assert!(files[0].to_str().unwrap().ends_with(".pdf"));
        assert!(h.backends.herdr.sends()[0].contains("Message: (no caption)\nAttachment: "));
    }

    #[test]
    fn file_follows_swipe_reply_target() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        session(&h, "ca", "w1:p3");
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        h.state
            .update_json(SENT_FILE, 0o600, |s: &mut Sent| {
                s.record(h.clock.0, 42, &[500], "ca");
                Ok(())
            })
            .unwrap();
        let api = FakeTelegram::new();
        api.add_download("d1", "documents/file_5.txt", b"x");
        let u = file_msg(9, |m| {
            m.document = Some(fref("d1"));
            m.reply_to_message = Some(crate::telegram::api::ReplyRef { message_id: 500 });
        });
        handle_file(&h.ctx(), &api, &u).unwrap().unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
        assert!(h.backends.herdr.sends()[0].starts_with("send w1:p3 "));
    }

    #[test]
    fn file_without_session_is_deleted_again() {
        let h = Harness::herdr();
        locked(&h);
        let api = FakeTelegram::new();
        api.add_download("d1", "documents/file_5.txt", b"x");
        let u = file_msg(9, |m| m.document = Some(fref("d1")));
        let r = handle_file(&h.ctx(), &api, &u).unwrap().unwrap();
        assert!(r.text.starts_with("\u{2753} no session selected"), "{}", r.text);
        assert!(inbox_files(&h).is_empty());
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn too_big_file_is_refused_without_download() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        let api = FakeTelegram::new();
        let u = file_msg(9, |m| {
            m.document = Some(FileRef { file_id: "x".into(), file_size: Some(25 * 1024 * 1024), ..Default::default() })
        });
        let r = handle_file(&h.ctx(), &api, &u).unwrap().unwrap();
        assert_eq!(r.text, "\u{26a0}\u{fe0f} file too big (max 20 MB)");
        assert!(api.file_requests.borrow().is_empty());
        assert!(inbox_files(&h).is_empty() && h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn file_from_stranger_or_group_is_ignored_and_not_downloaded() {
        let h = Harness::herdr();
        locked(&h);
        let api = FakeTelegram::new();
        api.add_download("d1", "documents/file_5.txt", b"x");
        let mut u = file_msg(9, |m| m.document = Some(fref("d1")));
        u.message.as_mut().unwrap().from = Some(User { id: 7 });
        assert!(handle_file(&h.ctx(), &api, &u).unwrap().is_none());
        let mut u = file_msg(9, |m| m.document = Some(fref("d1")));
        u.message.as_mut().unwrap().chat.kind = "group".to_string();
        assert!(handle_file(&h.ctx(), &api, &u).unwrap().is_none());
        assert!(api.file_requests.borrow().is_empty());
    }

    #[test]
    fn run_once_routes_file_messages() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        set_cfg(&h, |c| c.receiver = Some("bo".to_string()));
        let api = FakeTelegram::new();
        api.add_download("d1", "documents/file_5.txt", b"x");
        api.updates.borrow_mut().push_back(vec![file_msg(9, |m| m.document = Some(fref("d1")))]);
        let mut offset = 0;
        run_once(&h.ctx(), &api, &mut offset).unwrap();
        assert_eq!(api.sent.borrow()[0].1, "\u{1f4ce} sent to bo");
    }

    #[test]
    fn loop_cleans_inbox_at_start_and_does_not_replay_missed_jobs() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        let dir = h.state.path("inbox");
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.jpg");
        std::fs::write(&old, b"x").unwrap();
        let t: std::time::SystemTime = (h.clock.0 - chrono::Duration::days(9)).into();
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(t).unwrap();
        crate::state::schedule::add_job(
            &h.state,
            "m",
            "* * * * *",
            "UTC",
            crate::state::schedule::JobKind::Message { target: "bo".into(), message: "x".into() },
        )
        .unwrap();
        let fake = FakeTelegram::new();
        let polls = std::cell::Cell::new(0);
        let f = |_: &str| -> Box<dyn TelegramApi> {
            polls.set(polls.get() + 1);
            if polls.get() >= 2 {
                set_cfg(&h, |c| c.token = None);
            }
            Box::new(FakeTelegram::new())
        };
        let _ = &fake;
        let mut offset = 0;
        run_loop(&h.ctx(), &f, &mut offset, &|_| {});
        assert!(!old.exists());
        // the loop started after that minute began: nothing was fired
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn menu_has_only_list_and_use() {
        let names: Vec<&str> = BOT_COMMANDS.iter().map(|(c, _)| *c).collect();
        assert_eq!(names, vec!["list", "use"]);
    }
}
