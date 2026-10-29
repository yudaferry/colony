//! `colony send`. Telegram appears here only as the `Origin` envelope choice.

use super::log;
use crate::backend::{AgentState, Kind, detect_env};
use crate::cli::Origin;
use crate::ctx::Ctx;
use crate::envelope;
use crate::state::log::LogEvent;
use crate::state::names::{NAMES_FILE, Names};
use crate::target::resolve;
use anyhow::Result;
use std::time::Duration;

const DEFAULT_WAIT: Duration = Duration::from_secs(600);

pub fn send(
    ctx: &Ctx,
    message: &str,
    target: &str,
    wait: bool,
    from: Option<&Origin>,
) -> Result<String> {
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let mut t = resolve(target, &names)?;
    // An unnamed raw id of a Claude pane (e.g. an unregistered spawner) still gets the envelope.
    if t.kind == Kind::Other
        && t.name.is_none()
        && super::register::is_claude_pane(ctx.backends.get(t.backend), &t.pane_id)
    {
        t.kind = Kind::Claude;
    }

    // Sender: the caller's pane, by the backend its environment shows.
    let sender: Option<String> = detect_env(ctx.env).ok().and_then(|k| {
        let pane = ctx.backends.get(k).current_pane().ok()?;
        names.name_of_pane(k, &pane).map(str::to_string)
    });
    // Offer a reply instruction only to a Claude sender; a reply to a shell would be executed.
    let reply_to: Option<&str> = sender
        .as_deref()
        .filter(|n| names.get(n).is_some_and(|e| e.kind == Kind::Claude));

    let text = if t.kind == Kind::Claude {
        match from {
            Some(Origin::Telegram(id)) => envelope::telegram(id, message),
            None => envelope::laptop(sender.as_deref(), reply_to, message),
        }
    } else {
        message.to_string()
    };

    let backend = ctx.backends.get(t.backend);
    backend.send(&t, &text)?;

    let from_label = match from {
        Some(Origin::Telegram(id)) => format!("telegram:{id}"),
        None => sender.clone().unwrap_or_else(|| "user".to_string()),
    };
    log(
        ctx,
        &LogEvent::Send {
            from: from_label,
            to: t.name.clone().unwrap_or_else(|| target.to_string()),
            to_pane: t.pane_id.clone(),
            preview: message.to_string(),
        },
    );

    if wait {
        // tmux: the backend returns "herdr only" (after delivery, D-9).
        backend.wait(&t, AgentState::Idle, DEFAULT_WAIT)?;
    }
    Ok(format!("Sent to {target} ({})", t.pane_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendKind;
    use crate::commands::testutil::Harness;

    fn setup() -> Harness {
        let h = Harness::herdr();
        h.add_name("team-leader", BackendKind::Herdr, "w1:p1", Kind::Claude);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        h
    }

    #[test]
    fn raw_id_of_claude_pane_gets_envelope() {
        let h = Harness::herdr();
        h.backends.herdr.commands.borrow_mut().insert("w1:p9".into(), "claude".into());
        send(&h.ctx(), "hi", "w1:p9", false, None).unwrap();
        let want = format!("send w1:p9 {}", envelope::laptop(None, None, "hi"));
        assert_eq!(h.backends.herdr.sends(), vec![want]);
    }

    #[test]
    fn named_sender_gets_laptop_envelope() {
        let h = setup();
        send(&h.ctx(), "hi", "bo", false, None).unwrap();
        let want = format!("send w1:p2 {}", envelope::laptop(Some("team-leader"), Some("team-leader"), "hi"));
        assert_eq!(h.backends.herdr.sends(), vec![want]);
    }

    #[test]
    fn named_shell_sender_gets_no_instruction() {
        let h = Harness::herdr();
        h.add_name("me", BackendKind::Herdr, "w1:p1", Kind::Shell);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        send(&h.ctx(), "hi", "bo", false, None).unwrap();
        let sends = h.backends.herdr.sends();
        assert!(sends[0].contains("From: me"), "{sends:?}");
        assert!(!sends[0].contains("Instruction"), "{sends:?}");
        assert!(!sends[0].contains("colony send"), "{sends:?}");
    }

    #[test]
    fn unnamed_sender_gets_from_user() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        send(&h.ctx(), "hi", "bo", false, None).unwrap();
        let sends = h.backends.herdr.sends();
        assert!(sends[0].contains("From: user"), "{sends:?}");
        assert!(!sends[0].contains("Instruction"), "{sends:?}");
    }

    #[test]
    fn telegram_from_gives_telegram_envelope() {
        let h = setup();
        let o = Origin::Telegram("k3f9qa".to_string());
        send(&h.ctx(), "status?", "bo", false, Some(&o)).unwrap();
        let want = format!("send w1:p2 {}", envelope::telegram("k3f9qa", "status?"));
        assert_eq!(h.backends.herdr.sends(), vec![want]);
    }

    #[test]
    fn forwarded_keeps_same_reply_id() {
        let h = setup();
        // the caller (w1:p1) is the receiver session forwarding a Telegram message
        let o = Origin::Telegram("k3f9qa".to_string());
        send(&h.ctx(), "please check", "bo", false, Some(&o)).unwrap();
        let sends = h.backends.herdr.sends();
        assert!(sends[0].contains("reply-id k3f9qa"), "{sends:?}");
        assert!(sends[0].contains("colony telegram reply k3f9qa"), "{sends:?}");
    }

    #[test]
    fn shell_target_gets_raw_text() {
        let h = setup();
        h.add_name("sh", BackendKind::Herdr, "w1:p3", Kind::Shell);
        send(&h.ctx(), "ls -la", "sh", false, None).unwrap();
        assert_eq!(h.backends.herdr.sends(), vec!["send w1:p3 ls -la".to_string()]);
        assert!(!h.backends.herdr.sends()[0].contains("[COLONY MSG]"));
    }

    #[test]
    fn uses_stored_backend_not_env() {
        let h = setup();
        h.add_name("tw", BackendKind::Tmux, "%1", Kind::Claude);
        send(&h.ctx(), "hi", "tw", false, None).unwrap();
        assert_eq!(h.backends.tmux.sends().len(), 1);
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn wait_on_herdr_waits_idle() {
        let h = setup();
        send(&h.ctx(), "hi", "bo", true, None).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert!(log.iter().any(|l| l == "wait w1:p2 Idle 600s"), "{log:?}");
    }

    #[test]
    fn wait_on_tmux_is_herdr_only() {
        let h = setup();
        h.add_name("tw", BackendKind::Tmux, "%1", Kind::Claude);
        let err = send(&h.ctx(), "hi", "tw", true, None).unwrap_err();
        assert_eq!(err.to_string(), "herdr only");
        // delivered before the wait failed (D-9)
        assert_eq!(h.backends.tmux.sends().len(), 1);
    }

    #[test]
    fn send_logged_once() {
        let h = setup();
        send(&h.ctx(), "hi", "bo", false, None).unwrap();
        let log = h.log_text();
        assert_eq!(log.lines().filter(|l| l.contains(" SEND ")).count(), 1, "{log}");
        assert!(log.contains("from=team-leader to=bo pane=w1:p2"), "{log}");
    }

    #[test]
    fn unknown_target_errors_and_sends_nothing() {
        let h = setup();
        assert!(send(&h.ctx(), "hi", "ghost", false, None).is_err());
        assert!(h.backends.herdr.sends().is_empty());
    }
}
