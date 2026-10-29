//! The bot: a pure update handler (`handle_update`) and a thin polling loop.
//! Slash commands call core functions directly; no Claude session is involved (AG-4).
//! Everything sent to Telegram is an answer to a locked-user message or a reply
//! carrying a reply ID (FR-44, D-20).

use super::cmds::bot_id_of;
use super::api::{Message, TelegramApi, Update};
use super::reply_id::{PENDING_MODE, create_pending};
use super::split::{TG_LIMIT, split_message, tail_fit};
use crate::cli::Origin;
use crate::commands::list::list_rows;
use crate::commands::{backend_str, kind_str, read::read, send::send};
use crate::ctx::Ctx;
use crate::state::pending::{PENDING_FILE, Pending};
use crate::state::telegram::{TELEGRAM_FILE, TELEGRAM_MODE, TelegramConfig};
use crate::time::wib_secs;
use anyhow::Result;
use std::time::Duration;

const POLL_TIMEOUT_SECS: u64 = 30;
const RETRY_SLEEP: Duration = Duration::from_secs(5);
const DEFAULT_LAST_LINES: usize = 50;
const MAX_LAST_LINES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotReply {
    pub chat_id: i64,
    pub reply_to: i64,
    pub text: String,
}

const HELP: &str = "/list - list sessions\n\
/send <name> <msg> - send a message to a session\n\
/last <name> [lines] - show a session's recent output\n\
/help - this help\n\
Plain text goes to the receiver session, if one is set.";

/// Splits `s` at the first whitespace: (first word, trimmed rest).
fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((w, rest)) => (w, rest.trim()),
        None => (s, ""),
    }
}

/// Is `name` a registered session whose pane is still there (D-12)?
/// `list_rows` also drops stale names. A failing backend query counts as live.
fn session_live(ctx: &Ctx, name: &str) -> Result<bool> {
    let lname = name.to_lowercase();
    Ok(list_rows(ctx)?.iter().any(|r| r.name == lname))
}

/// New reply ID, then deliver with a Telegram envelope. A failed delivery
/// removes the ID again.
fn forward(ctx: &Ctx, msg: &Message, name: &str, text: &str) -> Result<Result<(), String>> {
    let lname = name.to_lowercase();
    let id = create_pending(ctx.state, ctx.clock.now(), &lname, msg.chat.id, msg.message_id)?;
    match send(ctx, text, &lname, false, Some(&Origin::Telegram(id.clone()))) {
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

fn cmd_list(ctx: &Ctx) -> Result<String> {
    let rows = list_rows(ctx)?;
    if rows.is_empty() {
        return Ok("no sessions registered".to_string());
    }
    Ok(rows
        .iter()
        .map(|r| {
            format!(
                "{} - {} - {} ({} {})",
                r.name,
                kind_str(r.kind),
                r.status,
                backend_str(r.backend),
                r.pane_id
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn cmd_send(ctx: &Ctx, msg: &Message, args: &str) -> Result<String> {
    let (name, text) = first_word(args);
    if name.is_empty() || text.is_empty() {
        return Ok("usage: /send <name> <message>".to_string());
    }
    if !session_live(ctx, name)? {
        return Ok(format!("no session named {}", name.to_lowercase()));
    }
    Ok(match forward(ctx, msg, name, text)? {
        Ok(()) => format!("sent to {}", name.to_lowercase()),
        Err(e) => format!("could not send to {}: {e}", name.to_lowercase()),
    })
}

fn cmd_last(ctx: &Ctx, args: &str) -> Result<String> {
    let (name, rest) = first_word(args);
    if name.is_empty() {
        return Ok("usage: /last <name> [lines]".to_string());
    }
    let lines = if rest.is_empty() {
        DEFAULT_LAST_LINES
    } else {
        match rest.parse::<usize>() {
            Ok(n) => n.clamp(1, MAX_LAST_LINES),
            Err(_) => return Ok("usage: /last <name> [lines]".to_string()),
        }
    };
    if !session_live(ctx, name)? {
        return Ok(format!("no session named {}", name.to_lowercase()));
    }
    Ok(match read(ctx, &name.to_lowercase(), lines) {
        Ok(out) => tail_fit(&out, TG_LIMIT),
        Err(e) => format!("could not read {}: {e:#}", name.to_lowercase()),
    })
}

fn cmd_plain(ctx: &Ctx, msg: &Message, cfg: &TelegramConfig, text: &str) -> Result<String> {
    let Some(receiver) = cfg.receiver.as_deref() else {
        return Ok("no receiver set, use /send".to_string());
    };
    if !session_live(ctx, receiver)? {
        return Ok(format!("receiver {receiver} not found, use /send"));
    }
    Ok(match forward(ctx, msg, receiver, text)? {
        Ok(()) => format!("sent to {receiver}"),
        Err(e) => format!("could not send to {receiver}: {e}"),
    })
}

/// Handles one update. `None` means: say nothing.
pub fn handle_update(ctx: &Ctx, u: &Update) -> Result<Option<BotReply>> {
    let Some(msg) = &u.message else {
        return Ok(None);
    };
    let Some(text) = &msg.text else {
        return Ok(None);
    };
    if msg.chat.kind != "private" {
        return Ok(None);
    }
    let Some(from) = &msg.from else {
        return Ok(None);
    };
    // Stale updates: Telegram replays up to 24 h of backlog. Anything sent before
    // the last connect is ignored entirely and never executed (D-15). `date` has
    // one-second resolution, so a message in the exact connect second cannot be told
    // apart from one sent just before it: `<=` drops it. A legitimate message in
    // that very second is lost (acceptable); the user simply sends it again.
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    if let Some(t) = cfg.connected_at {
        if msg.date <= t {
            return Ok(None);
        }
    }
    let say = |text: String| BotReply {
        chat_id: msg.chat.id,
        reply_to: msg.message_id,
        text,
    };

    // Lock: the first private message after connect locks the bot to its sender
    // and is not run. Without `connected_at` nobody can claim the lock.
    let locked_user = match cfg.locked_user_id {
        Some(id) => id,
        None => {
            if cfg.connected_at.is_none() {
                return Ok(None);
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
                return Ok(Some(say("locked to you, send /help".to_string())));
            }
            id
        }
    };
    if from.id != locked_user {
        return Ok(None);
    }

    let trimmed = text.trim();
    let reply = if let Some(cmdline) = trimmed.strip_prefix('/') {
        let (word, args) = first_word(cmdline);
        // `/list@colony_bot` -> `list`
        let command = word.split('@').next().unwrap_or("").to_lowercase();
        match command.as_str() {
            "list" => cmd_list(ctx)?,
            "send" => cmd_send(ctx, msg, args)?,
            "last" => cmd_last(ctx, args)?,
            _ => HELP.to_string(),
        }
    } else if trimmed.is_empty() {
        return Ok(None);
    } else {
        cmd_plain(ctx, msg, &cfg, trimmed)?
    };
    Ok(Some(say(reply)))
}

/// Sends a reply through `split_message`, `reply_to` on the first chunk only.
pub fn send_reply(api: &dyn TelegramApi, r: &BotReply) -> Result<()> {
    for (i, chunk) in split_message(&r.text, TG_LIMIT).iter().enumerate() {
        api.send_message(r.chat_id, chunk, (i == 0).then_some(r.reply_to))?;
    }
    Ok(())
}

fn warn(ctx: &Ctx, what: &str) {
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
/// `/send` is never re-run).
pub fn run_once_for(ctx: &Ctx, api: &dyn TelegramApi, bot: Option<i64>, offset: &mut i64) -> Result<()> {
    let updates = api.get_updates(*offset, POLL_TIMEOUT_SECS)?;
    for u in updates {
        let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
        if cfg.token.as_deref().and_then(bot_id_of) != bot {
            warn(ctx, "token changed during poll, dropping fetched updates");
            return Ok(());
        }
        *offset = (*offset).max(u.update_id + 1);
        // Persist before handling: a crash mid-update must not re-run a `/send`.
        persist_offset(ctx, bot, *offset);
        match handle_update(ctx, &u) {
            Ok(Some(r)) => {
                if let Err(e) = send_reply(api, &r) {
                    warn(ctx, &format!("could not answer update {}: {e:#}", u.update_id));
                }
            }
            Ok(None) => {}
            Err(e) => warn(ctx, &format!("update {} failed: {e:#}", u.update_id)),
        }
    }
    Ok(())
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
    let mut current_bot: Option<i64> = None;
    let mut registered = false;
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
        if let Err(e) = run_once_for(ctx, api.as_ref(), current_bot, offset) {
            // `e` comes from the API layer, which scrubs the token.
            warn(ctx, &format!("{e:#}"));
            sleep(RETRY_SLEEP);
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
    use crate::backend::{BackendKind, Kind};
    use crate::commands::testutil::Harness;
    use crate::telegram::api::{Chat, FakeTelegram, User};

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
            }),
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
        let r = handle_update(&h.ctx(), &upd(9, 42, 4242, "private", "/send bo hi"))
            .unwrap()
            .unwrap();
        assert_eq!(r.text, "locked to you, send /help");
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
        let none = Update { update_id: 1, message: None };
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
    fn send_creates_pending_and_delivers_telegram_envelope() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo-server", "w1:p2");
        let t = answer(&h, &priv_msg("/send bo-server status?"));
        assert_eq!(t, "sent to bo-server");
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
    fn send_works_without_receiver() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        assert_eq!(answer(&h, &priv_msg("/send bo hello there")), "sent to bo");
        assert!(h.backends.herdr.sends()[0].contains("Message: hello there"));
    }

    #[test]
    fn send_unknown_target_replies_error_no_pending() {
        let h = Harness::herdr();
        locked(&h);
        let t = answer(&h, &priv_msg("/send ghost hi"));
        assert_eq!(t, "no session named ghost");
        assert!(pending(&h).0.is_empty());
        assert!(h.backends.herdr.sends().is_empty());
        // A registered name whose pane is gone is also an error without pending.
        h.add_name("dead", BackendKind::Herdr, "w1:p77", Kind::Claude);
        assert_eq!(answer(&h, &priv_msg("/send dead hi")), "no session named dead");
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn failed_delivery_removes_pending() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        h.backends.herdr.fail_send.set(true);
        let t = answer(&h, &priv_msg("/send bo hi"));
        assert!(t.starts_with("could not send to bo"), "{t}");
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn last_trimmed_to_limit() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2");
        let text = (0..100)
            .map(|i| format!("{i:03}{}", "y".repeat(197)))
            .collect::<Vec<_>>()
            .join("\n");
        *h.backends.herdr.read_text.borrow_mut() = text;
        let t = answer(&h, &priv_msg("/last bo 100"));
        assert!(t.encode_utf16().count() <= TG_LIMIT);
        assert!(t.contains("099yyy"));
        assert!(!t.contains("000yyy"));
        assert!(h.backends.herdr.log.borrow().contains(&"read w1:p2 100".to_string()));
    }

    #[test]
    fn help_lists_commands() {
        let h = Harness::herdr();
        locked(&h);
        let t = answer(&h, &priv_msg("/help"));
        for want in ["/list", "/send <name> <msg>", "/last <name> [lines]", "/help", "Plain text"] {
            assert!(t.contains(want), "missing {want}: {t}");
        }
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
        assert_eq!(answer(&h, &priv_msg("how is it going")), "sent to leader");
        let sends = h.backends.herdr.sends();
        assert!(sends[0].contains("Message: how is it going"), "{sends:?}");
        assert_eq!(pending(&h).0.len(), 1);
    }

    #[test]
    fn plain_text_dead_receiver_replies_error_no_pending() {
        let h = Harness::herdr();
        locked(&h);
        set_cfg(&h, |c| c.receiver = Some("gone".to_string()));
        assert_eq!(answer(&h, &priv_msg("hi")), "receiver gone not found, use /send");
        assert!(pending(&h).0.is_empty());
    }

    #[test]
    fn plain_text_no_receiver() {
        let h = Harness::herdr();
        locked(&h);
        assert_eq!(answer(&h, &priv_msg("hi")), "no receiver set, use /send");
    }

    #[test]
    fn recognises_only_four_commands() {
        let h = Harness::herdr();
        locked(&h);
        let help = answer(&h, &priv_msg("/help"));
        for other in ["/start", "/foo bar", "/Settings", "/"] {
            assert_eq!(answer(&h, &priv_msg(other)), help, "{other}");
        }
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn run_once_advances_offset() {
        let h = Harness::herdr();
        locked(&h);
        let api = FakeTelegram::new();
        api.updates.borrow_mut().push_back(vec![priv_msg("/help")]);
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
        let mut stale = upd(1, 66, 66, "private", "/send bo rm -rf");
        stale.message.as_mut().unwrap().date = 1_999_999;
        assert_eq!(handle_update(&h.ctx(), &stale).unwrap(), None);
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, None);
        assert!(h.backends.herdr.sends().is_empty());
        assert!(pending(&h).0.is_empty());
        // A message after the connect second claims the lock.
        let mut fresh = upd(2, 77, 77, "private", "hi");
        fresh.message.as_mut().unwrap().date = 2_000_001;
        assert_eq!(answer(&h, &fresh), "locked to you, send /help");
        let cfg: TelegramConfig = h.state.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(cfg.locked_user_id, Some(77));
        // After the lock, a stale message from the locked user is still not run.
        let mut old = upd(3, 77, 77, "private", "/send bo x");
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
        set_cfg(&h, |c| c.offset_bot_id = Some(111));
        let api = FakeTelegram::new();
        api.updates.borrow_mut().push_back(vec![priv_msg("/send bo once")]);
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
            fn send_message(&self, chat: i64, text: &str, reply_to: Option<i64>) -> Result<()> {
                self.1.send_message(chat, text, reply_to)
            }
        }
        let fake = FakeTelegram::new();
        let mut stranger = upd(900, 66, 66, "private", "hello");
        stranger.message.as_mut().unwrap().date = 2_000_000;
        let mut owner = upd(901, 42, 42, "private", "/send bo x");
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
        let mut cmd = upd(2, 66, 66, "private", "/send bo x");
        cmd.message.as_mut().unwrap().date = 2_000_000;
        assert_eq!(handle_update(&h.ctx(), &cmd).unwrap(), None);
        assert!(h.backends.herdr.sends().is_empty());
    }
}
