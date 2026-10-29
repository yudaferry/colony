//! `colony telegram reply | connect | disconnect | status | receiver`.
//! The bot token never appears in output, errors or logs.

use super::api::TelegramApi;
use super::split::{TG_LIMIT, split_message};
use crate::backend::{Direction, detect_env};
use crate::commands::log;
use crate::ctx::Ctx;
use crate::state::log::LogEvent;
use crate::state::names::{Entry, Kind, NAMES_FILE, Names};
use crate::state::pending::{PENDING_FILE, Pending};
use crate::state::telegram::{TELEGRAM_FILE, TELEGRAM_MODE, TelegramConfig};
use crate::telegram::reply_id::PENDING_MODE;
use anyhow::{Result, anyhow, bail};
use std::path::Path;

/// Name under which the bot pane is registered.
pub const BOT_NAME: &str = "telegram-bot";

pub type ApiFactory<'a> = &'a dyn Fn(&str) -> Box<dyn TelegramApi>;

fn read_config(ctx: &Ctx) -> Result<TelegramConfig> {
    ctx.state.read_json(TELEGRAM_FILE)
}

/// `colony telegram reply <id> <text>`: any caller holding a valid ID may reply (FR-28).
/// Unknown/expired ID or no connection: error, no network call.
pub fn reply(ctx: &Ctx, api: &dyn TelegramApi, reply_id: &str, text: &str) -> Result<String> {
    let cfg = read_config(ctx)?;
    if cfg.token.is_none() {
        bail!("telegram is not connected. Run: colony telegram connect <token>");
    }
    let pending: Pending = ctx.state.read_json(PENDING_FILE)?;
    let Some(entry) = pending.get_valid(reply_id, ctx.clock.now()) else {
        bail!("reply id {reply_id} unknown or expired (valid 24 h)");
    };
    let chunks = split_message(text, TG_LIMIT);
    for (i, chunk) in chunks.iter().enumerate() {
        let reply_to = (i == 0).then_some(entry.message_id);
        api.send_message(entry.chat_id, chunk, reply_to)?;
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

pub fn connect(ctx: &Ctx, api_for: ApiFactory, token: &str, exe: &Path) -> Result<String> {
    let token = token.trim();
    let info = api_for(token)
        .get_me()
        .map_err(|e| anyhow!("could not validate the bot token: {e}"))?;

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

    let run_cmd = format!("{} telegram run", shell_word(&exe.display().to_string()));
    let mut out = format!("Connected as @{}.", info.username);

    let kind = match detect_env(ctx.env) {
        Ok(k) => k,
        Err(_) => {
            out.push_str(&format!(
                "\nNot inside herdr or tmux. Start the bot yourself with:\n{run_cmd}"
            ));
            return Ok(out);
        }
    };

    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    if bot_pane_alive(ctx, &names) == Some(true) {
        let pane = names.get(BOT_NAME).map(|e| e.pane_id.clone()).unwrap_or_default();
        out.push_str(&format!("\nBot already running in pane {pane}."));
        return Ok(out);
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
    out.push_str(&format!("\nBot started in pane {pane} (name: {BOT_NAME})."));
    Ok(out)
}

pub fn disconnect(ctx: &Ctx) -> Result<String> {
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let mut out = String::new();
    if let Some(e) = names.get(BOT_NAME) {
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
    out.push_str("Disconnected: token, lock, receiver and pending replies cleared.");
    Ok(out)
}

pub fn status(ctx: &Ctx, api_for: ApiFactory) -> Result<String> {
    let cfg = read_config(ctx)?;
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
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
    let pane = match bot_pane_alive(ctx, &names) {
        Some(true) => "alive",
        Some(false) => "not running",
        None => "unknown",
    };
    Ok(format!(
        "connected: {}\nbot: {bot}\nlocked user: {locked}\nreceiver: {receiver}\nbot pane: {pane}",
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
        assert_eq!(*api.sent.borrow(), vec![(42, "done".to_string(), Some(7))]);
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
        connect(&h.ctx(), &f, &format!("{id}:NEW"), exe()).unwrap();
        let c: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(c.connected_at, Some(h.ctx().clock.now().timestamp()));
        assert_eq!((c.offset, c.offset_bot_id), (0, Some(id)));
    }

    #[test]
    fn connect_invalid_token_saves_nothing() {
        let h = Harness::herdr();
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::rejecting("Unauthorized")) };
        let err = connect(&h.ctx(), &f, "999:BADTOKEN", exe()).unwrap_err().to_string();
        assert!(err.contains("Unauthorized"), "{err}");
        assert!(!err.contains("BADTOKEN"));
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.token, None);
        assert!(h.backends.herdr.log.borrow().is_empty());
    }

    #[test]
    fn connect_valid_saves_600_and_starts_bot_pane() {
        let h = Harness::herdr();
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let out = connect(&h.ctx(), &f, "111:SECRET", exe()).unwrap();
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
    }

    #[test]
    fn connect_no_multiplexer_prints_command() {
        let h = Harness::new(&[]);
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let out = connect(&h.ctx(), &f, "111:SECRET", exe()).unwrap();
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
        connect(&h.ctx(), &f, "111:NEW", exe()).unwrap();
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, Some(42));
        assert_eq!(cfg.token.as_deref(), Some("111:NEW"));
        // FakeTelegram reports bot id 111; a token that was saved for bot 222 is another bot.
        set_cfg(&h, |c| c.token = Some("222:OLD".to_string()));
        connect(&h.ctx(), &f, "111:NEW", exe()).unwrap();
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
    fn status_shows_five_items() {
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p9", None);
        h.add_name(BOT_NAME, BackendKind::Herdr, "w1:p9", Kind::Other);
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
        assert_eq!(lines[4], "bot pane: alive");
        assert!(!out.contains("SECRET"));
    }

    #[test]
    fn status_when_not_connected() {
        let h = Harness::herdr();
        let f = |_t: &str| -> Box<dyn TelegramApi> { Box::new(FakeTelegram::new()) };
        let out = status(&h.ctx(), &f).unwrap();
        assert_eq!(
            out,
            "connected: no\nbot: none\nlocked user: none\nreceiver: none\nbot pane: not running"
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
}
