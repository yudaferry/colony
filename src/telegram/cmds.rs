//! `colony telegram reply | connect | disconnect | status | receiver`.
//! The bot token never appears in output, errors or logs.

use super::api::{CAPTION_LIMIT, FileKind, TelegramApi};
use super::format::{send_markdown, sender_header, to_chunks};
use crate::discover::discover;
use crate::backend::{Direction, detect_env};
use crate::commands::log;
use crate::ctx::Ctx;
use crate::state::log::LogEvent;
use crate::state::names::{Entry, Kind, NAMES_FILE, Names};
use crate::state::pending::{PENDING_FILE, Pending};
use crate::state::sent::{SENT_FILE, SENT_MODE, Sent};
use crate::state::telegram::{TELEGRAM_FILE, TELEGRAM_MODE, TelegramConfig};
use crate::telegram::reply_id::PENDING_MODE;
use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};

/// Name under which the bot pane is registered.
pub const BOT_NAME: &str = "telegram-bot";

pub type ApiFactory<'a> = &'a dyn Fn(&str) -> Box<dyn TelegramApi>;

fn read_config(ctx: &Ctx) -> Result<TelegramConfig> {
    ctx.state.read_json(TELEGRAM_FILE)
}

/// A file to send with a reply (`--photo` / `--file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub kind: FileKind,
    pub path: PathBuf,
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Checks the file before any network call: exists, regular file, not empty, within the size limit.
fn validate_attachment(a: &Attachment) -> Result<()> {
    let flag = if a.kind == FileKind::Photo { "--photo" } else { "--file" };
    let shown = a.path.display();
    let meta = std::fs::metadata(&a.path).map_err(|e| anyhow!("{flag}: cannot read {shown}: {e}"))?;
    if !meta.is_file() {
        bail!("{flag}: {shown} is not a regular file");
    }
    if meta.len() == 0 {
        bail!("{flag}: {shown} is empty");
    }
    let max = a.kind.max_bytes();
    if meta.len() > max {
        bail!(
            "{flag}: {shown} is {} bytes, over the {} MB limit for {}",
            meta.len(),
            max / (1024 * 1024),
            if a.kind == FileKind::Photo { "photos" } else { "documents" }
        );
    }
    Ok(())
}

/// `colony telegram reply <id> <text>`: any caller holding a valid ID may reply (FR-28).
/// Unknown/expired ID or no connection: error, no network call.
pub fn reply(ctx: &Ctx, api: &dyn TelegramApi, reply_id: &str, text: &str) -> Result<String> {
    reply_with(ctx, api, reply_id, Some(text), None)
}

/// `reply` with an optional text and an optional photo/document.
pub fn reply_with(
    ctx: &Ctx,
    api: &dyn TelegramApi,
    reply_id: &str,
    text: Option<&str>,
    attach: Option<&Attachment>,
) -> Result<String> {
    let text = text.filter(|t| !t.trim().is_empty());
    if text.is_none() && attach.is_none() {
        bail!("give the answer text, or --photo <path> / --file <path>");
    }
    let cfg = read_config(ctx)?;
    if cfg.token.is_none() {
        bail!("telegram is not connected. Run: colony telegram connect <token>");
    }
    let pending: Pending = ctx.state.read_json(PENDING_FILE)?;
    let Some(entry) = pending.get_valid(reply_id, ctx.clock.now()) else {
        bail!("reply id {reply_id} unknown or expired (valid 24 h)");
    };
    if let Some(a) = attach {
        validate_attachment(a)?;
    }
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let (sender, target) = sender_name(ctx, &names, &entry.target);
    // A scheduled message has no phone message to quote (message_id 0).
    let quoted = (entry.message_id > 0).then_some(entry.message_id);
    let ids = match attach {
        None => send_markdown(api, entry.chat_id, text.unwrap_or_default(), quoted, Some(&sender))?,
        Some(a) => send_attachment(api, entry.chat_id, a, text, quoted, &sender)?,
    };
    // Remember who sent these messages, so a swipe-reply goes back to that session.
    // The messages are out already: a failed record is reported, not fatal.
    let chat_id = entry.chat_id;
    let recorded = if target.is_empty() {
        Ok(())
    } else {
        ctx.state.update_json(SENT_FILE, SENT_MODE, |s: &mut Sent| {
            s.record(ctx.clock.now(), chat_id, &ids, &target);
            Ok(())
        })
    };
    if let Err(e) = recorded {
        eprintln!("colony telegram: could not record the sent message: {e:#}");
    }
    log(
        ctx,
        &LogEvent::Reply {
            id: reply_id.to_string(),
            chat_id: entry.chat_id,
        },
    );
    Ok(format!("Sent to Telegram (reply id {reply_id})"))
}

/// Sends the file with the sender header and the text as caption. A text that does not fit the
/// caption limit goes out as normal messages after a header-only caption.
fn send_attachment(
    api: &dyn TelegramApi,
    chat_id: i64,
    a: &Attachment,
    text: Option<&str>,
    reply_to: Option<i64>,
    sender: &str,
) -> Result<Vec<i64>> {
    let (hh, hp) = sender_header(sender);
    let head_html = hh.trim_end().to_string();
    let head_plain = hp.trim_end().to_string();
    let mut caption: Option<(String, String)> = None;
    if let Some(t) = text {
        let room = CAPTION_LIMIT.saturating_sub(utf16_len(&hh).max(utf16_len(&hp)));
        let chunks = to_chunks(t, room);
        if let [c] = chunks.as_slice()
            && !c.plain_only
            && utf16_len(&format!("{hp}{}", c.plain)) <= CAPTION_LIMIT
        {
            caption = Some((format!("{hh}{}", c.html), format!("{hp}{}", c.plain)));
        }
    }
    let (html, plain, rest) = match caption {
        Some((h, p)) => (h, p, None),
        None => (head_html, head_plain, text),
    };
    let first = match api.send_file(chat_id, a.kind, &a.path, &html, true, reply_to) {
        Ok(id) => id,
        Err(_) => api.send_file(chat_id, a.kind, &a.path, &plain, false, reply_to)?,
    };
    let mut ids = vec![first];
    if let Some(t) = rest {
        ids.extend(send_markdown(api, chat_id, t, None, None)?);
    }
    Ok(ids)
}

/// Name for the reply header: the calling pane's registered name, else its discovered
/// display name, else its pane id. When the pane is unknown: the pending target, else "colony".
/// Returns `(header name, resolvable target)`; the target is the registered name, the
/// discovered name when it is unique, else the pane id.
fn sender_name(ctx: &Ctx, names: &Names, target: &str) -> (String, String) {
    let fallback = if target.trim().is_empty() { "colony" } else { target };
    let fb = || (fallback.to_string(), target.trim().to_string());
    let Ok(kind) = detect_env(ctx.env) else {
        return fb();
    };
    let backend = ctx.backends.get(kind);
    let Ok(pane) = backend.current_pane() else {
        return fb();
    };
    if let Some(n) = names.name_of_pane(kind, &pane) {
        return (n.to_string(), n.to_string());
    }
    let found = backend.list_panes().ok().and_then(|ps| {
        let all = discover(ctx, kind, &ps, names);
        let d = all.iter().find(|d| d.pane_id == pane)?;
        let unique = all.iter().filter(|o| o.name == d.name).count() == 1;
        Some((d.name.clone(), if unique { d.name.clone() } else { d.pane_id.clone() }))
    });
    found.unwrap_or_else(|| (pane.clone(), pane))
}

/// Bot id is the part of the token before `:`.
pub fn bot_id_of(token: &str) -> Option<i64> {
    token.split(':').next()?.parse().ok()
}

/// Quotes a word for typing into a shell, only when needed.
fn shell_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+@:%".contains(&b));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// True when `telegram-bot` is registered and its pane is live. `None` if the
/// backend could not be asked.
fn bot_pane_alive(ctx: &Ctx, names: &Names) -> Option<bool> {
    let Some(e) = names.get(BOT_NAME) else {
        return Some(false);
    };
    match ctx.backends.get(e.backend).list_panes() {
        Ok(ps) => Some(ps.iter().any(|p| p.pane_id == e.pane_id)),
        Err(_) => None,
    }
}

/// Result of `connect`: the text to print, and whether the caller must now run
/// the bot loop in the current pane.
#[derive(Debug)]
pub struct Connected {
    pub message: String,
    pub run_here: bool,
}

/// Where a live bot runs, for messages.
fn bot_where(cfg: &TelegramConfig, pid: u32) -> String {
    match &cfg.bot_pane {
        Some(p) => format!("pid {pid}, pane {p}"),
        None => format!("pid {pid}"),
    }
}

pub fn connect(
    ctx: &Ctx,
    api_for: ApiFactory,
    token: &str,
    exe: &Path,
    new_pane: bool,
) -> Result<Connected> {
    let token = token.trim();
    let api = api_for(token);
    let info = api
        .get_me()
        .map_err(|e| anyhow!("could not validate the bot token: {e}"))?;
    // The `/` menu is set before the bot runs; failure is only logged.
    super::bot::register_commands(ctx, api.as_ref());

    let before = read_config(ctx)?;
    let live = before.live_bot_pid();
    if let Some(pid) = live
        && before.token.as_deref() == Some(token)
    {
        bail!(
            "a bot for this token is already running ({}). Stop it with: colony telegram disconnect",
            bot_where(&before, pid)
        );
    }

    // D-17: keep the lock if it is the same bot, else clear it.
    ctx.state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            let old_id = c.token.as_deref().and_then(bot_id_of);
            if old_id != Some(info.id) {
                c.locked_user_id = None;
                c.locked_chat_id = None;
            }
            if c.offset_bot_id != Some(info.id) {
                c.offset = 0;
                c.offset_bot_id = Some(info.id);
            }
            c.token = Some(token.to_string());
            c.connected_at = Some(ctx.clock.now().timestamp());
            Ok(())
        })?;

    let mut out = format!("Connected as @{}.", info.username);

    // Another bot is alive on an older token: its loop picks the new token up.
    if let Some(pid) = live {
        out.push_str(&format!(
            "\nBot already running ({}); it switches to this token.",
            bot_where(&before, pid)
        ));
        return Ok(Connected { message: out, run_here: false });
    }

    let run_cmd = format!("{} telegram run", shell_word(&exe.display().to_string()));

    if !new_pane {
        ctx.state
            .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                c.bot_pane = None;
                Ok(())
            })?;
        return Ok(Connected {
            message: format!(
                "Connected as @{} \u{2014} bot running here, Ctrl+C to stop",
                info.username
            ),
            run_here: true,
        });
    }

    let kind = match detect_env(ctx.env) {
        Ok(k) => k,
        Err(_) => {
            out.push_str(&format!(
                "\nNot inside herdr or tmux. Start the bot yourself with:\n{run_cmd}"
            ));
            return Ok(Connected { message: out, run_here: false });
        }
    };

    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    if bot_pane_alive(ctx, &names) == Some(true) {
        let pane = names.get(BOT_NAME).map(|e| e.pane_id.clone()).unwrap_or_default();
        out.push_str(&format!("\nBot already running in pane {pane}."));
        return Ok(Connected { message: out, run_here: false });
    }

    let backend = ctx.backends.get(kind);
    let from = backend.current_pane()?;
    let pane = backend.split(&from, Direction::Right, None)?;
    backend.run_program(&pane, &run_cmd)?;
    let entry = Entry {
        backend: kind,
        pane_id: pane.clone(),
        kind: Kind::Other,
        cwd: None,
        created_at: ctx.clock.now(),
        spawned_by: None,
    };
    ctx.state.update_json(NAMES_FILE, 0o644, |n: &mut Names| {
        n.insert(BOT_NAME, entry);
        Ok(())
    })?;
    ctx.state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            c.bot_pane = Some(pane.clone());
            Ok(())
        })?;
    out.push_str(&format!("\nBot started in pane {pane} (name: {BOT_NAME})."));
    Ok(Connected { message: out, run_here: false })
}

pub fn disconnect(ctx: &Ctx) -> Result<String> {
    let cfg = read_config(ctx)?;
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let mut out = String::new();
    // Only a pane colony created (`connect --new-pane`) is ever closed.
    if let Some(e) = names.get(BOT_NAME)
        && cfg.bot_pane.as_deref() == Some(e.pane_id.as_str())
    {
        // The pane may already be gone: stopping it is best effort.
        match ctx.backends.get(e.backend).close(&e.pane_id) {
            Ok(()) => out.push_str(&format!("Closed bot pane {}.\n", e.pane_id)),
            Err(_) => out.push_str(&format!("Bot pane {} was already gone.\n", e.pane_id)),
        }
        ctx.state.update_json(NAMES_FILE, 0o644, |n: &mut Names| {
            n.remove(BOT_NAME);
            Ok(())
        })?;
    }
    // Token, lock and receiver go (D-13), and pending reply IDs.
    ctx.state
        .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            *c = TelegramConfig::default();
            Ok(())
        })?;
    ctx.state
        .update_json(PENDING_FILE, PENDING_MODE, |p: &mut Pending| {
            *p = Pending::default();
            Ok(())
        })?;
    ctx.state.update_json(SENT_FILE, SENT_MODE, |s: &mut Sent| {
        *s = Sent::default();
        Ok(())
    })?;
    out.push_str("Disconnected: token, lock, receiver and pending replies cleared.");
    Ok(out)
}

pub fn status(ctx: &Ctx, api_for: ApiFactory) -> Result<String> {
    let cfg = read_config(ctx)?;
    let connected = cfg.token.is_some();
    let bot = match cfg.token.as_deref() {
        None => "none".to_string(),
        Some(t) => match api_for(t).get_me() {
            Ok(b) => format!("@{}", b.username),
            Err(e) => format!("unavailable ({e})"),
        },
    };
    let locked = cfg
        .locked_user_id
        .map_or_else(|| "none".to_string(), |id| id.to_string());
    let receiver = cfg.receiver.clone().unwrap_or_else(|| "none".to_string());
    let process = match cfg.live_bot_pid() {
        Some(pid) => format!("running ({})", bot_where(&cfg, pid)),
        None => "not running".to_string(),
    };
    Ok(format!(
        "connected: {}\nbot: {bot}\nlocked user: {locked}\nreceiver: {receiver}\nbot process: {process}",
        if connected { "yes" } else { "no" }
    ))
}

pub fn receiver(ctx: &Ctx, name: Option<&str>, clear: bool) -> Result<String> {
    match (name, clear) {
        (None, true) => {
            ctx.state
                .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                    c.receiver = None;
                    Ok(())
                })?;
            Ok("Receiver cleared".to_string())
        }
        (Some(n), false) => {
            let n = n.to_lowercase();
            ctx.state
                .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                    c.receiver = Some(n.clone());
                    Ok(())
                })?;
            Ok(format!("Receiver set: {n}"))
        }
        _ => bail!("give exactly one of: a session name, or --clear"),
    }
}

/// `receiver`, then refreshes the pinned session bar through the saved token. Not connected or
/// not locked: skipped. A bar failure is reported on stderr and never fails the change.
pub fn receiver_pinned(ctx: &Ctx, api_for: ApiFactory, name: Option<&str>, clear: bool) -> Result<String> {
    let out = receiver(ctx, name, clear)?;
    if let Ok(cfg) = read_config(ctx) {
        if let Some(token) = cfg.token.as_deref() {
            let api = api_for(token);
            if let Err(e) = crate::telegram::bot::sync_pin(ctx, api.as_ref()) {
                eprintln!("colony telegram: could not update the pinned session bar: {e:#}");
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendKind;
    use crate::commands::testutil::Harness;
    use crate::state::pending::PendingEntry;
    use crate::telegram::api::FakeTelegram;
    use chrono::Duration;
    use std::os::unix::fs::PermissionsExt;

    fn set_cfg(h: &Harness, f: impl FnOnce(&mut TelegramConfig)) {
        h.state
            .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                f(c);
                Ok(())
            })
            .unwrap();
    }

    fn connected(h: &Harness) {
        set_cfg(h, |c| c.token = Some("111:SECRET".to_string()));
    }

    fn add_pending(h: &Harness, id: &str, age: Duration) {
        let created_at = h.clock.0 - age;
        h.state
            .update_json(PENDING_FILE, PENDING_MODE, |p: &mut Pending| {
                p.0.insert(
                    id.to_string(),
                    PendingEntry {
                        target: "bo".to_string(),
                        chat_id: 42,
                        message_id: 7,
                        created_at,
                    },
                );
                Ok(())
            })
            .unwrap();
    }

    /// Expected plain-text header for an upper-case name.
    fn hdr(name: &str) -> String {
        format!("\u{1f916} {name}\n{}\n\n", "\u{2501}".repeat(14))
    }

    fn hdr_html(name: &str) -> String {
        format!("\u{1f916} <b>{name}</b>\n{}\n\n", "\u{2501}".repeat(14))
    }

    #[test]
    fn header_uses_registered_name() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        h.add_name("bo-server", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "**ok** done").unwrap();
        assert_eq!(
            *api.html_sent.borrow(),
            vec![(42, format!("{}<b>ok</b> done", hdr_html("BO-SERVER")), Some(7))]
        );
    }

    #[test]
    fn header_uses_discovered_name() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("77.json"), r#"{"pid":77,"sessionId":"x","name":"report-server"}"#).unwrap();
        let h = Harness::new(&[
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "w1:p1"),
            ("HOME", home.path().to_str().unwrap()),
        ]);
        h.backends.herdr.panes.borrow_mut().clear();
        h.backends.herdr.add_claude("w1:p1", crate::backend::AgentState::Working, None);
        h.backends.herdr.pids.borrow_mut().insert("w1:p1".into(), vec![77]);
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "hi").unwrap();
        assert_eq!(api.sent.borrow()[0].1, format!("{}hi", hdr_html("REPORT-SERVER")));
    }

    #[test]
    fn header_falls_back_to_pending_target_then_colony() {
        let h = Harness::new(&[]);
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "hi").unwrap();
        assert_eq!(api.sent.borrow()[0].1, format!("{}hi", hdr_html("BO")));
        assert_eq!(sender_name(&h.ctx(), &h.names(), ""), ("colony".to_string(), String::new()));
    }

    #[test]
    fn header_only_on_first_chunk() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        h.add_name("bo", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let api = FakeTelegram::new();
        let text = (0..1500).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        reply(&h.ctx(), &api, "k3f9qa", &text).unwrap();
        let sent = api.sent.borrow();
        assert!(sent.len() > 1);
        assert!(sent[0].1.starts_with(&hdr_html("BO")));
        assert!(sent[1..].iter().all(|m| !m.1.contains('\u{1f916}') && !m.1.contains('\u{2501}')));
        assert!(sent.iter().all(|m| m.1.encode_utf16().count() <= 4096));
    }

    #[test]
    fn html_fallback_keeps_header() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        h.add_name("bo", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let api = FakeTelegram::new();
        api.reject_html.set(true);
        reply(&h.ctx(), &api, "k3f9qa", "**x**").unwrap();
        assert_eq!(api.sent.borrow()[0].1, format!("{}**x**", hdr("BO")));
    }

    #[test]
    fn header_name_is_escaped() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        h.add_name("a<b>&**c", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "hi").unwrap();
        let m = &api.sent.borrow()[0].1;
        assert!(m.starts_with("\u{1f916} <b>A&lt;B&gt;&amp;**C</b>\n"), "{m}");
        assert_eq!(m.matches("<b>").count(), 1);
    }

    fn tmp_file(name: &str, size: usize) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(name);
        std::fs::write(&p, vec![7u8; size]).unwrap();
        (d, p)
    }

    fn setup_reply() -> Harness {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        h.add_name("bo", BackendKind::Herdr, "w1:p1", Kind::Claude);
        h
    }

    fn photo(p: &Path) -> Attachment {
        Attachment { kind: FileKind::Photo, path: p.to_path_buf() }
    }

    #[test]
    fn photo_with_short_caption() {
        let h = setup_reply();
        let (_d, p) = tmp_file("a.png", 100);
        let api = FakeTelegram::new();
        reply_with(&h.ctx(), &api, "k3f9qa", Some("**ok** done"), Some(&photo(&p))).unwrap();
        let files = api.files.borrow();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, FileKind::Photo);
        assert_eq!(files[0].path, p);
        assert_eq!(files[0].caption, format!("{}<b>ok</b> done", hdr_html("BO")));
        assert!(files[0].html);
        assert_eq!((files[0].chat_id, files[0].reply_to), (42, Some(7)));
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn photo_without_text_has_header_only_caption() {
        let h = setup_reply();
        let (_d, p) = tmp_file("a.jpg", 10);
        let api = FakeTelegram::new();
        reply_with(&h.ctx(), &api, "k3f9qa", None, Some(&photo(&p))).unwrap();
        assert_eq!(api.files.borrow()[0].caption, hdr_html("BO").trim_end());
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn long_caption_becomes_header_caption_plus_text() {
        let h = setup_reply();
        let (_d, p) = tmp_file("a.png", 10);
        let api = FakeTelegram::new();
        let text = "a".repeat(1500);
        reply_with(&h.ctx(), &api, "k3f9qa", Some(&text), Some(&photo(&p))).unwrap();
        let files = api.files.borrow();
        assert_eq!(files[0].caption, hdr_html("BO").trim_end());
        assert!(utf16_len(&files[0].caption) <= CAPTION_LIMIT);
        let sent = api.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, text);
        assert_eq!(sent[0].2, None);
    }

    #[test]
    fn caption_html_rejected_falls_back_to_plain() {
        let h = setup_reply();
        let (_d, p) = tmp_file("a.png", 10);
        let api = FakeTelegram::new();
        api.reject_html.set(true);
        reply_with(&h.ctx(), &api, "k3f9qa", Some("**x**"), Some(&photo(&p))).unwrap();
        let files = api.files.borrow();
        assert_eq!(files.len(), 1);
        assert!(!files[0].html);
        assert_eq!(files[0].caption, format!("{}**x**", hdr("BO")));
    }

    #[test]
    fn bad_files_error_without_network() {
        let h = setup_reply();
        let d = tempfile::tempdir().unwrap();
        let api = FakeTelegram::new();
        let missing = photo(&d.path().join("nope.png"));
        let err = reply_with(&h.ctx(), &api, "k3f9qa", Some("x"), Some(&missing)).unwrap_err().to_string();
        assert!(err.contains("--photo") && err.contains("nope.png"), "{err}");
        let err = reply_with(&h.ctx(), &api, "k3f9qa", None, Some(&photo(d.path()))).unwrap_err().to_string();
        assert!(err.contains("not a regular file"), "{err}");
        let (_d2, big) = tmp_file("big.png", 10 * 1024 * 1024 + 1);
        let err = reply_with(&h.ctx(), &api, "k3f9qa", None, Some(&photo(&big))).unwrap_err().to_string();
        assert!(err.contains("over the 10 MB limit"), "{err}");
        // The same file is fine as a document (limit 50 MB).
        let doc = Attachment { kind: FileKind::Document, path: big.clone() };
        reply_with(&h.ctx(), &api, "k3f9qa", None, Some(&doc)).unwrap();
        assert_eq!(api.files.borrow().len(), 1);
        let (_d3, empty) = tmp_file("e.png", 0);
        assert!(reply_with(&h.ctx(), &api, "k3f9qa", None, Some(&photo(&empty))).is_err());
        assert_eq!(api.files.borrow().len(), 1);
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn no_text_and_no_file_is_an_error() {
        let h = setup_reply();
        let api = FakeTelegram::new();
        assert!(reply_with(&h.ctx(), &api, "k3f9qa", None, None).is_err());
        assert!(reply_with(&h.ctx(), &api, "k3f9qa", Some("  "), None).is_err());
    }

    #[test]
    fn file_uses_send_document() {
        let h = setup_reply();
        let (_d, p) = tmp_file("report.pdf", 10);
        let api = FakeTelegram::new();
        let a = Attachment { kind: FileKind::Document, path: p };
        reply_with(&h.ctx(), &api, "k3f9qa", Some("see attached"), Some(&a)).unwrap();
        let files = api.files.borrow();
        assert_eq!(files[0].kind, FileKind::Document);
        assert_eq!(files[0].caption, format!("{}see attached", hdr_html("BO")));
    }

    #[test]
    fn file_send_records_ids_in_sent_json() {
        let h = setup_reply();
        let (_d, p) = tmp_file("a.png", 10);
        let api = FakeTelegram::new();
        reply_with(&h.ctx(), &api, "k3f9qa", Some(&"b".repeat(1500)), Some(&photo(&p))).unwrap();
        let ids: Vec<i64> = api.ids.borrow().iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids.len(), 2);
        let sent: Sent = h.state.read_json(SENT_FILE).unwrap();
        for id in ids {
            assert_eq!(sent.target_of(42, id, h.clock.0), Some("bo"));
        }
        assert_eq!(mode(&h, SENT_FILE), 0o600);
    }

    fn exe() -> &'static Path {
        Path::new("/usr/bin/colony")
    }

    fn mode(h: &Harness, file: &str) -> u32 {
        std::fs::metadata(h.state.path(file)).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn reply_sends_to_recorded_chat_and_message() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(5));
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "done").unwrap();
        assert_eq!(*api.sent.borrow(), vec![(42, format!("{}done", hdr_html("W1:P1")), Some(7))]);
    }

    #[test]
    fn reply_records_every_chunk_in_sent_json_mode_600() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(5));
        h.add_name("bo-server", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", &"a".repeat(10_000)).unwrap();
        let ids: Vec<i64> = api.ids.borrow().iter().map(|(id, _, _)| *id).collect();
        assert!(ids.len() >= 3, "{ids:?}");
        let sent: Sent = h.state.read_json(SENT_FILE).unwrap();
        assert_eq!(sent.0.len(), ids.len());
        for id in ids {
            assert_eq!(sent.target_of(42, id, h.clock.0), Some("bo-server"));
        }
        assert_eq!(mode(&h, SENT_FILE), 0o600);
    }

    #[test]
    fn reply_records_pane_id_target_and_prunes_old_entries() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(5));
        h.state
            .update_json(SENT_FILE, SENT_MODE, |s: &mut Sent| {
                s.record(h.clock.0 - Duration::days(8), 42, &[1], "old");
                s.record(h.clock.0 - Duration::days(6), 42, &[2], "recent");
                Ok(())
            })
            .unwrap();
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "hi").unwrap();
        let sent: Sent = h.state.read_json(SENT_FILE).unwrap();
        assert!(!sent.0.contains_key("42:1"));
        assert!(sent.0.contains_key("42:2"));
        // Header said W1:P1 (unregistered pane): the stored target is the pane id.
        let id = api.ids.borrow()[0].0;
        assert_eq!(sent.target_of(42, id, h.clock.0), Some("w1:p1"));
    }

    #[test]
    fn reply_twice_within_24h_both_deliver() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::hours(23));
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "progress").unwrap();
        reply(&h.ctx(), &api, "k3f9qa", "final").unwrap();
        assert_eq!(api.sent.borrow().len(), 2);
    }

    #[test]
    fn reply_expired_id_errors_without_network() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::hours(25));
        let api = FakeTelegram::new();
        let err = reply(&h.ctx(), &api, "k3f9qa", "late").unwrap_err().to_string();
        assert!(err.contains("unknown or expired"), "{err}");
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn reply_unknown_id_errors_without_network() {
        let h = Harness::herdr();
        connected(&h);
        let api = FakeTelegram::new();
        let err = reply(&h.ctx(), &api, "nope00", "x").unwrap_err().to_string();
        assert!(err.contains("reply id nope00 unknown or expired (valid 24 h)"), "{err}");
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn reply_not_connected_errors() {
        let h = Harness::herdr();
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        let api = FakeTelegram::new();
        let err = reply(&h.ctx(), &api, "k3f9qa", "x").unwrap_err().to_string();
        assert!(err.contains("not connected"), "{err}");
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn any_caller_may_reply() {
        for h in [
            Harness::new(&[]),
            Harness::new(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w9:p9")]),
        ] {
            connected(&h);
            add_pending(&h, "k3f9qa", Duration::minutes(1));
            let api = FakeTelegram::new();
            reply(&h.ctx(), &api, "k3f9qa", "hi").unwrap();
            assert_eq!(api.sent.borrow().len(), 1);
        }
    }

    #[test]
    fn long_reply_split() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", &"a".repeat(10_000)).unwrap();
        let sent = api.sent.borrow();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0].2, Some(7));
        assert_eq!(sent[1].2, None);
        assert_eq!(sent[2].2, None);
    }

    #[test]
    fn reply_logged() {
        let h = Harness::herdr();
        connected(&h);
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        let api = FakeTelegram::new();
        reply(&h.ctx(), &api, "k3f9qa", "hi").unwrap();
        let log = h.log_text();
        assert!(log.contains("REPLY id=k3f9qa chat_id=42"), "{log}");
        assert!(!log.contains("SECRET"));
    }

    #[test]
    fn connect_records_connected_at_and_resets_offset_for_new_bot() {
        let h = Harness::herdr();
        let id = FakeTelegram::new().me.as_ref().unwrap().id;
        set_cfg(&h, |c| {
            c.token = Some("5:OLD".to_string());
            c.offset = 40;
            c.offset_bot_id = Some(5);
        });
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        connect(&h.ctx(), &f, &format!("{id}:NEW"), exe(), true).unwrap();
        let c: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(c.connected_at, Some(h.ctx().clock.now().timestamp()));
        assert_eq!((c.offset, c.offset_bot_id), (0, Some(id)));
    }

    #[test]
    fn connect_invalid_token_saves_nothing() {
        let h = Harness::herdr();
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::rejecting("Unauthorized")) };
        let err = connect(&h.ctx(), &f, "999:BADTOKEN", exe(), true).unwrap_err().to_string();
        assert!(err.contains("Unauthorized"), "{err}");
        assert!(!err.contains("BADTOKEN"));
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.token, None);
        assert!(h.backends.herdr.log.borrow().is_empty());
    }

    #[test]
    fn connect_registers_command_menu_and_survives_failure() {
        use std::rc::Rc;
        struct Shared(Rc<FakeTelegram>);
        impl TelegramApi for Shared {
            fn get_me(&self) -> Result<super::super::api::BotInfo> {
                self.0.get_me()
            }
            fn get_updates(&self, o: i64, t: u64) -> Result<Vec<super::super::api::Update>> {
                self.0.get_updates(o, t)
            }
            fn send_message(&self, c: i64, t: &str, r: Option<i64>) -> Result<i64> {
                self.0.send_message(c, t, r)
            }
            fn set_my_commands(&self, c: &[(&str, &str)]) -> Result<()> {
                self.0.set_my_commands(c)
            }
        }
        let fake = Rc::new(FakeTelegram::new());
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(Shared(fake.clone())) };
        let h = Harness::herdr();
        connect(&h.ctx(), &f, "111:SECRET", exe(), false).unwrap();
        assert_eq!(fake.commands.borrow().len(), 1);
        let h2 = Harness::herdr();
        fake.fail_commands.set(true);
        assert!(connect(&h2.ctx(), &f, "111:SECRET", exe(), false).unwrap().run_here);
    }

    #[test]
    fn connect_valid_saves_600_and_starts_bot_pane() {
        let h = Harness::herdr();
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let done = connect(&h.ctx(), &f, "111:SECRET", exe(), true).unwrap();
        assert!(!done.run_here);
        let out = done.message;
        assert!(out.contains("@colony_bot"), "{out}");
        assert!(!out.contains("SECRET"));
        assert_eq!(mode(&h, TELEGRAM_FILE), 0o600);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.token.as_deref(), Some("111:SECRET"));
        let log = h.backends.herdr.log.borrow().clone();
        assert!(log.iter().any(|l| l.starts_with("split w1:p1 Right")), "{log:?}");
        assert!(
            log.iter().any(|l| l.starts_with("run w1:p100 ") && l.contains("telegram run")),
            "{log:?}"
        );
        let e = h.names().get(BOT_NAME).cloned().unwrap();
        assert_eq!(e.pane_id, "w1:p100");
        assert_eq!(e.kind, Kind::Other);
        assert_eq!(e.backend, BackendKind::Herdr);
        assert_eq!(cfg.bot_pane.as_deref(), Some("w1:p100"));
    }

    #[test]
    fn connect_default_runs_here_without_split_or_name() {
        let h = Harness::herdr();
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let done = connect(&h.ctx(), &f, "111:SECRET", exe(), false).unwrap();
        assert!(done.run_here);
        assert!(done.message.contains("@colony_bot"), "{}", done.message);
        assert!(done.message.contains("Ctrl+C"), "{}", done.message);
        assert!(!done.message.contains("SECRET"));
        assert!(h.backends.herdr.log.borrow().is_empty(), "{:?}", h.backends.herdr.log.borrow());
        assert!(h.names().0.is_empty());
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.token.as_deref(), Some("111:SECRET"));
        assert_eq!(cfg.bot_pane, None);
        assert_eq!(mode(&h, TELEGRAM_FILE), 0o600);
    }

    #[test]
    fn connect_default_works_without_multiplexer() {
        let h = Harness::new(&[]);
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let done = connect(&h.ctx(), &f, "111:SECRET", exe(), false).unwrap();
        assert!(done.run_here);
    }

    fn live_pid(h: &Harness, pane: Option<&str>) {
        set_cfg(h, |c| {
            c.token = Some("111:SECRET".to_string());
            c.bot_pid = Some(std::process::id());
            c.bot_pane = pane.map(str::to_string);
        });
    }

    #[test]
    fn connect_refuses_second_bot_for_same_token() {
        let h = Harness::herdr();
        live_pid(&h, Some("w1:p7"));
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        for new_pane in [false, true] {
            let err = connect(&h.ctx(), &f, "111:SECRET", exe(), new_pane)
                .err()
                .unwrap()
                .to_string();
            assert!(err.contains("already running"), "{err}");
            assert!(err.contains(&format!("pid {}", std::process::id())), "{err}");
            assert!(err.contains("pane w1:p7"), "{err}");
            assert!(!err.contains("SECRET"));
        }
        assert!(h.backends.herdr.log.borrow().is_empty());
    }

    #[test]
    fn connect_with_dead_recorded_pid_starts_normally() {
        let h = Harness::herdr();
        set_cfg(&h, |c| {
            c.token = Some("111:SECRET".to_string());
            c.bot_pid = Some(4_000_000_000);
        });
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        assert!(connect(&h.ctx(), &f, "111:SECRET", exe(), false).unwrap().run_here);
    }

    #[test]
    fn connect_new_token_while_bot_alive_does_not_start_second() {
        let h = Harness::herdr();
        live_pid(&h, None);
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let done = connect(&h.ctx(), &f, "111:NEWTOKEN", exe(), false).unwrap();
        assert!(!done.run_here);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.token.as_deref(), Some("111:NEWTOKEN"));
    }

    #[test]
    fn connect_no_multiplexer_prints_command() {
        let h = Harness::new(&[]);
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let out = connect(&h.ctx(), &f, "111:SECRET", exe(), true).unwrap().message;
        assert!(out.contains("/usr/bin/colony telegram run"), "{out}");
        assert!(h.names().0.is_empty());
        assert_eq!(mode(&h, TELEGRAM_FILE), 0o600);
    }

    #[test]
    fn connect_same_bot_keeps_lock_other_bot_clears_it() {
        let h = Harness::new(&[]);
        set_cfg(&h, |c| {
            c.token = Some("111:OLD".to_string());
            c.locked_user_id = Some(42);
            c.locked_chat_id = Some(42);
        });
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        connect(&h.ctx(), &f, "111:NEW", exe(), false).unwrap();
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, Some(42));
        assert_eq!(cfg.token.as_deref(), Some("111:NEW"));
        // FakeTelegram reports bot id 111; a token that was saved for bot 222 is another bot.
        set_cfg(&h, |c| c.token = Some("222:OLD".to_string()));
        connect(&h.ctx(), &f, "111:NEW", exe(), false).unwrap();
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, None);
        assert_eq!(cfg.locked_chat_id, None);
    }

    #[test]
    fn disconnect_closes_pane_and_clears_token_and_lock() {
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p9", None);
        h.add_name(BOT_NAME, BackendKind::Herdr, "w1:p9", Kind::Other);
        set_cfg(&h, |c| {
            c.token = Some("111:SECRET".to_string());
            c.locked_user_id = Some(42);
            c.locked_chat_id = Some(42);
            c.receiver = Some("bo".to_string());
        });
        add_pending(&h, "k3f9qa", Duration::minutes(1));
        set_cfg(&h, |c| c.bot_pane = Some("w1:p9".to_string()));
        let out = disconnect(&h.ctx()).unwrap();
        assert!(!out.contains("SECRET"));
        assert!(h.backends.herdr.log.borrow().contains(&"close w1:p9".to_string()));
        assert!(h.names().get(BOT_NAME).is_none());
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg, TelegramConfig::default());
        let p: Pending = h.state.read_json(PENDING_FILE).unwrap();
        assert!(p.0.is_empty());
        assert_eq!(mode(&h, TELEGRAM_FILE), 0o600);
    }

    #[test]
    fn disconnect_never_closes_a_pane_colony_did_not_create() {
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p9", None);
        // Registered under the bot name by someone else; no bot_pane recorded.
        h.add_name(BOT_NAME, BackendKind::Herdr, "w1:p9", Kind::Other);
        connected(&h);
        disconnect(&h.ctx()).unwrap();
        assert!(!h.backends.herdr.log.borrow().iter().any(|l| l.starts_with("close")));
        assert!(h.names().get(BOT_NAME).is_some());
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg, TelegramConfig::default());
    }

    #[test]
    fn disconnect_here_bot_closes_nothing() {
        let h = Harness::herdr();
        live_pid(&h, None);
        let out = disconnect(&h.ctx()).unwrap();
        assert!(!out.contains("Closed"), "{out}");
        assert!(h.backends.herdr.log.borrow().is_empty());
    }

    #[test]
    fn status_running_with_live_pid_and_pane() {
        let h = Harness::herdr();
        live_pid(&h, Some("w1:p9"));
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let out = status(&h.ctx(), &f).unwrap();
        assert!(
            out.ends_with(&format!("bot process: running (pid {}, pane w1:p9)", std::process::id())),
            "{out}"
        );
        live_pid(&h, None);
        let out = status(&h.ctx(), &f).unwrap();
        assert!(
            out.ends_with(&format!("bot process: running (pid {})", std::process::id())),
            "{out}"
        );
    }

    #[test]
    fn status_not_running_for_dead_pid() {
        let h = Harness::herdr();
        set_cfg(&h, |c| {
            c.token = Some("111:SECRET".to_string());
            c.bot_pid = Some(4_000_000_000);
        });
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        assert!(status(&h.ctx(), &f).unwrap().ends_with("bot process: not running"));
    }

    #[test]
    fn status_not_running_for_non_colony_pid() {
        let h = Harness::herdr();
        // PID 1 is alive but is not a colony process.
        set_cfg(&h, |c| {
            c.token = Some("111:SECRET".to_string());
            c.bot_pid = Some(1);
        });
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        assert!(status(&h.ctx(), &f).unwrap().ends_with("bot process: not running"));
    }

    #[test]
    fn status_shows_five_items() {
        let h = Harness::herdr();
        set_cfg(&h, |c| {
            c.token = Some("111:SECRET".to_string());
            c.locked_user_id = Some(42);
            c.receiver = Some("bo".to_string());
        });
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let out = status(&h.ctx(), &f).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5, "{out}");
        assert_eq!(lines[0], "connected: yes");
        assert_eq!(lines[1], "bot: @colony_bot");
        assert_eq!(lines[2], "locked user: 42");
        assert_eq!(lines[3], "receiver: bo");
        assert_eq!(lines[4], "bot process: not running");
        assert!(!out.contains("SECRET"));
    }

    #[test]
    fn status_when_not_connected() {
        let h = Harness::herdr();
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let out = status(&h.ctx(), &f).unwrap();
        assert_eq!(
            out,
            "connected: no\nbot: none\nlocked user: none\nreceiver: none\nbot process: not running"
        );
    }

    #[test]
    fn receiver_set_and_clear_reflected_in_status() {
        let h = Harness::herdr();
        connected(&h);
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        receiver(&h.ctx(), Some("Team-Leader"), false).unwrap();
        assert!(status(&h.ctx(), &f).unwrap().contains("receiver: team-leader"));
        receiver(&h.ctx(), None, true).unwrap();
        assert!(status(&h.ctx(), &f).unwrap().contains("receiver: none"));
        assert!(receiver(&h.ctx(), None, false).is_err());
        assert!(receiver(&h.ctx(), Some("a"), true).is_err());
    }

    #[test]
    fn receiver_keeps_mode_600() {
        let h = Harness::herdr();
        connected(&h);
        receiver(&h.ctx(), Some("bo"), false).unwrap();
        assert_eq!(mode(&h, TELEGRAM_FILE), 0o600);
        receiver(&h.ctx(), None, true).unwrap();
        assert_eq!(mode(&h, TELEGRAM_FILE), 0o600);
    }

    #[test]
    fn shell_word_quotes_only_when_needed() {
        assert_eq!(shell_word("/usr/bin/colony"), "/usr/bin/colony");
        assert_eq!(shell_word("/my dir/colony"), "'/my dir/colony'");
        assert_eq!(shell_word("a'b"), "'a'\\''b'");
    }

    #[test]
    fn cli_receiver_updates_the_pinned_bar() {
        use std::rc::Rc;
        struct Shared(Rc<FakeTelegram>);
        impl TelegramApi for Shared {
            fn get_me(&self) -> Result<super::super::api::BotInfo> {
                self.0.get_me()
            }
            fn get_updates(&self, o: i64, t: u64) -> Result<Vec<super::super::api::Update>> {
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
        connected(&h);
        let fake = Rc::new(FakeTelegram::new());
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(Shared(fake.clone())) };
        // Not locked: skipped, the change still happens.
        receiver_pinned(&h.ctx(), &f, Some("bo"), false).unwrap();
        assert!(fake.ids.borrow().is_empty());
        set_cfg(&h, |c| {
            c.locked_user_id = Some(42);
            c.locked_chat_id = Some(42);
        });
        receiver_pinned(&h.ctx(), &f, Some("Team"), false).unwrap();
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        let id = cfg.pinned_message_id.unwrap();
        assert!(fake.ids.borrow().iter().any(|(i, c, t)| *i == id && *c == 42 && t == "\u{1f4cc} CURRENT SESSION: TEAM"));
        assert_eq!(fake.pins.borrow().as_slice(), &[(42, id)]);
        receiver_pinned(&h.ctx(), &f, None, true).unwrap();
        assert_eq!(fake.unpins.borrow().as_slice(), &[(42, id)]);
        assert_eq!(fake.deletes.borrow().as_slice(), &[(42, id)]);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        let new = cfg.pinned_message_id.unwrap();
        assert_ne!(new, id);
        assert!(fake.ids.borrow().iter().any(|(i, _, t)| *i == new && t == "\u{1f4cc} CURRENT SESSION: none"));
        assert_eq!(mode(&h, TELEGRAM_FILE), 0o600);
    }
}
