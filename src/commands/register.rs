//! `colony register <name> [pane-id]`.

use super::log;
use crate::backend::{Kind, detect_env};
use crate::ctx::Ctx;
use crate::state::log::LogEvent;
use crate::state::names::{Entry, NAMES_FILE, Names};
use anyhow::Result;

/// D-4: kind from the pane's foreground program. herdr gives the detected agent
/// kind (no agent = a plain shell pane); tmux gives `#{pane_current_command}`.
fn detect_kind(cmd: Option<&str>, herdr: bool) -> Kind {
    detect_kind_with(cmd, herdr, || false)
}

/// Like `detect_kind`; when the foreground program is a shell (Claude runs under a
/// `zsh -c` wrapper in tmux), `screen_is_claude` is asked before settling on shell.
fn detect_kind_with(cmd: Option<&str>, herdr: bool, screen_is_claude: impl Fn() -> bool) -> Kind {
    match cmd {
        None if herdr => Kind::Shell,
        None => Kind::Other,
        Some(c) => match c.trim().trim_start_matches('-') {
            "claude" => Kind::Claude,
            "zsh" | "bash" | "sh" if screen_is_claude() => Kind::Claude,
            "zsh" | "bash" | "sh" => Kind::Shell,
            _ => Kind::Other,
        },
    }
}

/// True when the pane's foreground program is detected as Claude (same rule as `register`).
pub fn is_claude_pane(backend: &dyn crate::backend::Backend, pane: &str) -> bool {
    let cmd = backend.pane_command(pane).unwrap_or(None);
    detect_kind_with(cmd.as_deref(), true, || backend.screen_is_claude(pane)) == Kind::Claude
}

pub fn register(ctx: &Ctx, name: &str, pane_id: Option<&str>) -> Result<String> {
    let backend_kind = detect_env(ctx.env)?;
    let backend = ctx.backends.get(backend_kind);
    let pane = match pane_id {
        Some(p) => p.to_string(),
        None => backend.current_pane()?,
    };
    let command = backend.pane_command(&pane).unwrap_or(None);
    let kind = detect_kind_with(
        command.as_deref(),
        backend_kind == crate::backend::BackendKind::Herdr,
        || backend.screen_is_claude(&pane),
    );
    if kind == Kind::Claude {
        // herdr: the colony name is also the agent name. tmux: no-op.
        backend.name_agent(&pane, &name.to_lowercase())?;
    }
    let entry = Entry {
        backend: backend_kind,
        pane_id: pane.clone(),
        kind,
        cwd: None,
        created_at: ctx.clock.now(),
        spawned_by: None,
    };
    ctx.state.update_json(NAMES_FILE, 0o644, |n: &mut Names| {
        n.insert(name, entry);
        Ok(())
    })?;
    let name = name.to_lowercase();
    log(
        ctx,
        &LogEvent::Register {
            name: name.clone(),
            pane: pane.clone(),
        },
    );
    Ok(format!("Registered: {name} \u{2192} {pane}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendKind;
    use crate::commands::testutil::Harness;

    #[test]
    fn registers_current_pane_by_default() {
        let h = Harness::herdr();
        let out = register(&h.ctx(), "Foo", None).unwrap();
        assert_eq!(out, "Registered: foo \u{2192} w1:p1");
        let e = h.names().get("foo").unwrap().clone();
        assert_eq!(e.pane_id, "w1:p1");
        assert_eq!(e.backend, BackendKind::Herdr);
    }

    #[test]
    fn registers_given_pane() {
        let h = Harness::herdr();
        register(&h.ctx(), "foo", Some("w1:p7")).unwrap();
        assert_eq!(h.names().get("foo").unwrap().pane_id, "w1:p7");
    }

    #[test]
    fn herdr_claude_pane_is_renamed() {
        let h = Harness::herdr();
        h.backends
            .herdr
            .commands
            .borrow_mut()
            .insert("w1:p1".to_string(), "claude".to_string());
        register(&h.ctx(), "foo", None).unwrap();
        assert!(h.backends.herdr.log.borrow().contains(&"name w1:p1 foo".to_string()));
        assert_eq!(h.names().get("foo").unwrap().kind, Kind::Claude);
    }

    #[test]
    fn kind_detected_per_d4() {
        assert_eq!(detect_kind(Some("claude"), true), Kind::Claude);
        assert_eq!(detect_kind(None, true), Kind::Shell);
        assert_eq!(detect_kind(Some("opencode"), true), Kind::Other);
        assert_eq!(detect_kind(Some("claude"), false), Kind::Claude);
        assert_eq!(detect_kind(Some("zsh"), false), Kind::Shell);
        assert_eq!(detect_kind(Some("bash"), false), Kind::Shell);
        assert_eq!(detect_kind(Some("sh"), false), Kind::Shell);
        assert_eq!(detect_kind(Some("vim"), false), Kind::Other);
        assert_eq!(detect_kind(None, false), Kind::Other);

        let h = Harness::new(&[("TMUX", "/tmp/x,1,0")]);
        h.backends
            .tmux
            .commands
            .borrow_mut()
            .insert("%1".to_string(), "zsh".to_string());
        register(&h.ctx(), "t", None).unwrap();
        let e = h.names().get("t").unwrap().clone();
        assert_eq!(e.backend, BackendKind::Tmux);
        assert_eq!(e.kind, Kind::Shell);
    }

    #[test]
    fn tmux_shell_wrapper_with_claude_screen_is_claude() {
        let h = Harness::new(&[("TMUX", "/tmp/x,1,0")]);
        h.backends.tmux.commands.borrow_mut().insert("%2".to_string(), "zsh".to_string());
        h.backends.tmux.claude_screens.borrow_mut().push("%2".to_string());
        register(&h.ctx(), "bo", Some("%2")).unwrap();
        assert_eq!(h.names().get("bo").unwrap().kind, Kind::Claude);
        assert!(is_claude_pane(&h.backends.tmux, "%2"));
    }

    #[test]
    fn tmux_plain_shell_stays_shell() {
        let h = Harness::new(&[("TMUX", "/tmp/x,1,0")]);
        h.backends.tmux.commands.borrow_mut().insert("%2".to_string(), "zsh".to_string());
        register(&h.ctx(), "sh", Some("%2")).unwrap();
        assert_eq!(h.names().get("sh").unwrap().kind, Kind::Shell);
    }

    #[test]
    fn register_in_tmux_with_herdr_env_uses_tmux() {
        let h = Harness::new(&[("TMUX", "/tmp/x,1,0"), ("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w8:pZ")]);
        register(&h.ctx(), "lead", None).unwrap();
        let e = h.names().get("lead").unwrap().clone();
        assert_eq!((e.backend, e.pane_id.as_str()), (BackendKind::Tmux, "%1"));
    }

    #[test]
    fn no_env_errors() {
        let h = Harness::new(&[]);
        let msg = register(&h.ctx(), "foo", None).unwrap_err().to_string();
        assert!(msg.contains("HERDR_ENV") && msg.contains("TMUX"), "{msg}");
        assert!(h.names().0.is_empty());
    }

    #[test]
    fn no_main_state_written() {
        let h = Harness::herdr();
        register(&h.ctx(), "foo", None).unwrap();
        let text = std::fs::read_to_string(h.state.path("names.json")).unwrap();
        assert!(!text.to_lowercase().contains("main"), "{text}");
        assert!(!h.state.path("main").exists());
    }

    #[test]
    fn register_logged() {
        let h = Harness::herdr();
        register(&h.ctx(), "foo", None).unwrap();
        let log = h.log_text();
        assert!(log.contains("REGISTER name=foo pane=w1:p1"), "{log}");
    }
}
