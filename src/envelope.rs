//! Message envelopes: laptop, Telegram, and the intro sent by `spawn`.

/// Envelope for a message sent from a colony session (or the user's own pane).
/// `reply_to` is set only when the sender is a Claude pane; a shell sender keeps
/// its From line but gets no instruction (a reply would be typed into the shell).
pub fn laptop(sender: Option<&str>, reply_to: Option<&str>, text: &str) -> String {
    match (sender, reply_to) {
        (Some(name), Some(r)) => format!(
            "[COLONY MSG]\nFrom: {name}\nMessage: {text}\nInstruction: reply only if useful, with: colony send \"<reply>\" {r}\n[/COLONY MSG]"
        ),
        (Some(name), None) => format!("[COLONY MSG]\nFrom: {name}\nMessage: {text}\n[/COLONY MSG]"),
        (None, _) => format!("[COLONY MSG]\nFrom: user\nMessage: {text}\n[/COLONY MSG]"),
    }
}

/// Envelope for a message that came from the user's phone.
pub fn telegram(reply_id: &str, text: &str) -> String {
    format!(
        "[COLONY MSG]\nFrom: telegram (reply-id {reply_id})\nMessage: {text}\nInstruction: This came from the user's phone. Keep the answer short. Reply phone-friendly: result first, then one fact per line starting with •, no long paragraphs; use a few fitting emoji (✅ done, ❌ failed, ⏳ in progress, ⚠️ warning, 📌 next step). Send your answer with: colony telegram reply {reply_id} \"<answer>\"\n[/COLONY MSG]"
    )
}

/// First message of a spawned Claude session.
/// `spawner` is the displayed name; `reply_to` is what `colony send` can resolve
/// (the spawner's name or raw pane id), or `None` to omit the reply instruction.
pub fn intro(name: &str, pane_id: &str, spawner: &str, reply_to: Option<&str>) -> String {
    let reply = match reply_to {
        Some(r) => format!(" Reply only if useful, with: colony send \"<reply>\" {r}"),
        None => String::new(),
    };
    format!(
        "You are colony agent '{name}' (pane {pane_id}). The orchestrator is '{spawner}'.\n\
Messages to you arrive wrapped in [COLONY MSG] ... [/COLONY MSG] and come in two kinds.\n\
1. From: <session name>. Another colony session wrote it.{reply}\n\
2. From: telegram (reply-id <id>). The user wrote it from the phone. Keep the answer short and send it with: colony telegram reply <id> \"<answer>\"\n\
Do not answer a Telegram message in the terminal; only the colony telegram reply command reaches the phone."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn laptop_named_sender() {
        assert_eq!(
            laptop(Some("team-leader"), Some("team-leader"), "hi"),
            "[COLONY MSG]\nFrom: team-leader\nMessage: hi\nInstruction: reply only if useful, with: colony send \"<reply>\" team-leader\n[/COLONY MSG]"
        );
    }

    #[test]
    fn laptop_shell_sender_has_from_but_no_instruction() {
        assert_eq!(
            laptop(Some("me"), None, "hi"),
            "[COLONY MSG]\nFrom: me\nMessage: hi\n[/COLONY MSG]"
        );
    }

    #[test]
    fn laptop_unnamed_sender() {
        assert_eq!(
            laptop(None, None, "hi"),
            "[COLONY MSG]\nFrom: user\nMessage: hi\n[/COLONY MSG]"
        );
    }

    #[test]
    fn telegram_envelope() {
        assert_eq!(
            telegram("k3f9qa", "status?"),
            "[COLONY MSG]\nFrom: telegram (reply-id k3f9qa)\nMessage: status?\nInstruction: This came from the user's phone. Keep the answer short. Reply phone-friendly: result first, then one fact per line starting with •, no long paragraphs; use a few fitting emoji (✅ done, ❌ failed, ⏳ in progress, ⚠️ warning, 📌 next step). Send your answer with: colony telegram reply k3f9qa \"<answer>\"\n[/COLONY MSG]"
        );
    }

    #[test]
    fn intro_mentions_everything() {
        let t = intro("bo-server", "w8:p2", "team-leader", Some("team-leader"));
        for want in [
            "bo-server",
            "w8:p2",
            "team-leader",
            "[COLONY MSG]",
            "From: telegram (reply-id",
            "colony send \"<reply>\" team-leader",
            "colony telegram reply <id> \"<answer>\"",
        ] {
            assert!(t.contains(want), "missing {want:?} in {t}");
        }
    }

    #[test]
    fn intro_unnamed_spawner_uses_pane_id() {
        let t = intro("bo-server", "w8:p2", "user", Some("w1:p1"));
        assert!(t.contains("The orchestrator is 'user'."));
        assert!(t.contains("colony send \"<reply>\" w1:p1"));
        assert!(!t.contains("\" user"));
    }

    #[test]
    fn intro_no_reply_target_omits_instruction() {
        let t = intro("bo-server", "w8:p2", "user", None);
        assert!(!t.contains("colony send"));
        assert!(t.contains("colony telegram reply <id>"));
    }
}
