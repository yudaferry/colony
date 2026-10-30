//! Auto-discovery of Claude Code sessions that are not registered (read only:
//! nothing found here is ever written to `names.json`).
//!
//! Display name of a discovered session, best source first:
//! 1. `~/.claude/sessions/<pid>.json` `name` of the Claude process in the pane. Claude Code
//!    keeps this file per process; `name` is the `/rename` name (or the auto-derived one).
//! 2. The terminal title, only when it looks like a name (one word after stripping a
//!    spinner glyph and quotes). A task summary such as "Merging test-bo into prod-bo-a"
//!    is not a name.
//! 3. The pane id.

use crate::backend::{AgentState, Backend, BackendKind, Kind, PaneInfo, PaneRef};
use crate::commands::register::detect_kind_with;
use crate::ctx::{Ctx, Env};
use crate::state::names::Names;
use crate::target;
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Discovered {
    pub backend: BackendKind,
    pub pane_id: String,
    /// Session name, or the pane id when no name could be found.
    pub name: String,
    pub state: Option<AgentState>,
    pub cwd: Option<String>,
}

const BACKENDS: [BackendKind; 2] = [BackendKind::Herdr, BackendKind::Tmux];

fn sessions_dir(env: &dyn Env) -> Option<PathBuf> {
    let base = match env.var("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env.var("HOME").filter(|v| !v.is_empty())?).join(".claude"),
    };
    Some(base.join("sessions"))
}

/// Removes matching surrounding quotes (`"x"`, `'x'`, curly) and whitespace.
fn unquote(s: &str) -> &str {
    let quotes = ['"', '\'', '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}'];
    s.trim().trim_matches(&quotes[..]).trim()
}

/// `name` from the Claude session file of one of `pids`.
fn session_file_name(env: &dyn Env, pids: &[u32]) -> Option<String> {
    let dir = sessions_dir(env)?;
    pids.iter().find_map(|pid| {
        let text = std::fs::read_to_string(dir.join(format!("{pid}.json"))).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        if v.get("pid").and_then(Value::as_u64) != Some(u64::from(*pid)) {
            return None;
        }
        let name = unquote(v.get("name")?.as_str()?);
        (!name.is_empty()).then(|| name.to_string())
    })
}

/// A terminal title that is a session name: no spinner prefix, no quotes, one word.
pub fn title_as_name(title: &str) -> Option<String> {
    let mut t = title.trim();
    // Spinner / status glyph: a first word that has no letter or digit, e.g. "✳" or "◐".
    if let Some((first, rest)) = t.split_once(char::is_whitespace)
        && !first.chars().any(char::is_alphanumeric)
        && !first.chars().any(|c| "\"'\u{201c}\u{2018}".contains(c))
    {
        t = rest.trim();
    }
    let t = unquote(t);
    (!t.is_empty() && t.chars().count() <= 64 && !t.contains(char::is_whitespace))
        .then(|| t.to_string())
}

fn is_claude(backend: &dyn Backend, p: &PaneInfo) -> bool {
    if let Some((agent, _)) = &p.agent {
        return agent == "claude";
    }
    // No agent info (tmux, or a herdr pane without an agent): the foreground program.
    match p.command.as_deref() {
        None => false,
        Some(c) => {
            detect_kind_with(Some(c), false, || backend.screen_is_claude(&p.pane_id)) == Kind::Claude
        }
    }
}

/// Unregistered Claude sessions among `panes` (the one `list_panes` answer of `backend`).
pub fn discover(ctx: &Ctx, backend: BackendKind, panes: &[PaneInfo], names: &Names) -> Vec<Discovered> {
    let b = ctx.backends.get(backend);
    panes
        .iter()
        .filter(|p| names.name_of_pane(backend, &p.pane_id).is_none())
        .filter(|p| is_claude(b, p))
        .map(|p| {
            let name = session_file_name(ctx.env, &b.claude_pids(p))
                .or_else(|| p.title.as_deref().and_then(title_as_name))
                .unwrap_or_else(|| p.pane_id.clone());
            Discovered {
                backend,
                pane_id: p.pane_id.clone(),
                name,
                state: p.agent.as_ref().map(|(_, s)| *s),
                cwd: p.cwd.clone(),
            }
        })
        .collect()
}

/// Like `target::resolve`, and when that finds nothing: a unique discovered display name.
pub fn resolve(ctx: &Ctx, arg: &str, names: &Names) -> Result<PaneRef> {
    let err = match target::resolve(arg, names) {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    let mut found: Vec<Discovered> = Vec::new();
    for backend in BACKENDS {
        // An unreachable backend is skipped.
        if let Ok(panes) = ctx.backends.get(backend).list_panes() {
            found.extend(
                discover(ctx, backend, &panes, names)
                    .into_iter()
                    .filter(|d| d.name.to_lowercase() == arg.to_lowercase()),
            );
        }
    }
    match found.as_slice() {
        [] => Err(err),
        [d] => Ok(PaneRef {
            backend: d.backend,
            pane_id: d.pane_id.clone(),
            name: None,
            kind: Kind::Claude,
        }),
        many => Err(anyhow!(
            "'{arg}' matches {} Claude sessions: {}; use a pane id",
            many.len(),
            many.iter()
                .map(|d| format!("{} ({})", d.pane_id, crate::commands::backend_str(d.backend)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::list::{list, list_rows};
    use crate::commands::testutil::Harness;
    use crate::commands::{read::read, send::send};
    use crate::envelope;

    /// Harness (caller in herdr pane w1:p1) whose HOME holds the given
    /// `.claude/sessions/<pid>.json` files.
    fn harness(sessions: &[(u32, &str)]) -> (Harness, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        for (pid, name) in sessions {
            std::fs::write(
                dir.join(format!("{pid}.json")),
                format!(r#"{{"pid":{pid},"sessionId":"x","name":{}}}"#, serde_json::to_string(name).unwrap()),
            )
            .unwrap();
        }
        let h = Harness::new(&[
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "w1:p1"),
            ("HOME", home.path().to_str().unwrap()),
        ]);
        (h, home)
    }

    #[test]
    fn title_rules() {
        assert_eq!(title_as_name("\"team-leader\"").as_deref(), Some("team-leader"));
        assert_eq!(title_as_name("\u{2733} report-server").as_deref(), Some("report-server"));
        assert_eq!(title_as_name("\u{25d0} \"team-leader\"").as_deref(), Some("team-leader"));
        assert_eq!(title_as_name("Merging test-bo into prod-bo-a"), None);
        assert_eq!(title_as_name("\u{2733} Merging test-bo"), None);
        assert_eq!(title_as_name("  "), None);
    }

    #[test]
    fn herdr_unregistered_claude_shown_with_session_name() {
        let (h, _home) = harness(&[(4242, "\"team-leader\"")]);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, Some("Merging a into b"));
        h.backends.herdr.pids.borrow_mut().insert("w1:p5".into(), vec![1, 4242]);
        let rows = list_rows(&h.ctx()).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!((r.name.as_str(), r.pane_id.as_str()), ("team-leader", "w1:p5"));
        assert!(!r.registered);
        assert_eq!(r.status, "idle");
        let out = list(&h.ctx()).unwrap();
        assert!(out.contains("claude*"), "{out}");
        assert!(out.trim_end().ends_with("* not registered"), "{out}");
        assert!(h.names().0.is_empty(), "discovery must not write names.json");
    }

    #[test]
    fn session_file_beats_title() {
        let (h, _home) = harness(&[(7, "from-file")]);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, Some("from-title"));
        h.backends.herdr.pids.borrow_mut().insert("w1:p5".into(), vec![7]);
        assert_eq!(list_rows(&h.ctx()).unwrap()[0].name, "from-file");
    }

    #[test]
    fn name_like_title_used_without_session_file() {
        let (h, _home) = harness(&[]);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, Some("\"report-server\""));
        assert_eq!(list_rows(&h.ctx()).unwrap()[0].name, "report-server");
    }

    #[test]
    fn summary_title_falls_back_to_pane_id() {
        let (h, _home) = harness(&[]);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, Some("Merging test-bo into prod-bo-a"));
        assert_eq!(list_rows(&h.ctx()).unwrap()[0].name, "w1:p5");
    }

    #[test]
    fn registered_panes_not_duplicated_and_shells_ignored() {
        let (h, _home) = harness(&[]);
        h.backends.herdr.add_claude("w1:p2", AgentState::Idle, Some("bo"));
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        let rows = list_rows(&h.ctx()).unwrap();
        assert_eq!(rows.len(), 1, "w1:p1 is a plain shell, w1:p2 is registered");
        assert!(rows[0].registered);
        let out = list(&h.ctx()).unwrap();
        assert!(!out.contains("not registered"), "{out}");
    }

    #[test]
    fn send_and_read_by_discovered_name() {
        let (h, _home) = harness(&[(9, "worker-x")]);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, None);
        h.backends.herdr.pids.borrow_mut().insert("w1:p5".into(), vec![9]);
        send(&h.ctx(), "hi", "Worker-X", false, None).unwrap();
        let want = format!("send w1:p5 {}", envelope::laptop(None, None, "hi"));
        assert_eq!(h.backends.herdr.sends(), vec![want]);
        *h.backends.herdr.read_text.borrow_mut() = "out".into();
        assert_eq!(read(&h.ctx(), "worker-x", 10).unwrap(), "out");
        assert!(h.names().0.is_empty());
    }

    #[test]
    fn registered_name_wins_over_discovered_name() {
        let (h, _home) = harness(&[(9, "bo")]);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, None);
        h.backends.herdr.pids.borrow_mut().insert("w1:p5".into(), vec![9]);
        let r = resolve(&h.ctx(), "bo", &h.names()).unwrap();
        assert_eq!(r.pane_id, "w1:p2");
    }

    #[test]
    fn raw_id_still_works_for_discovered_pane() {
        let (h, _home) = harness(&[]);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, None);
        let r = resolve(&h.ctx(), "w1:p5", &h.names()).unwrap();
        assert_eq!(r.pane_id, "w1:p5");
    }

    #[test]
    fn ambiguous_name_errors_with_pane_ids() {
        let (h, _home) = harness(&[(1, "dup"), (2, "dup")]);
        h.backends.herdr.add_claude("w1:p5", AgentState::Idle, None);
        h.backends.herdr.add_claude("w1:p6", AgentState::Idle, None);
        h.backends.herdr.pids.borrow_mut().insert("w1:p5".into(), vec![1]);
        h.backends.herdr.pids.borrow_mut().insert("w1:p6".into(), vec![2]);
        let msg = send(&h.ctx(), "hi", "dup", false, None).unwrap_err().to_string();
        assert!(msg.contains("w1:p5") && msg.contains("w1:p6"), "{msg}");
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn unknown_name_keeps_register_hint() {
        let (h, _home) = harness(&[]);
        let msg = read(&h.ctx(), "ghost", 5).unwrap_err().to_string();
        assert!(msg.contains("no pane registered with name 'ghost'"), "{msg}");
    }

    #[test]
    fn tmux_claude_by_command_and_by_screen() {
        let (h, _home) = harness(&[(30, "tmux-claude")]);
        h.backends.tmux.panes.borrow_mut().clear();
        h.backends.tmux.add_command_pane("%3", "claude", Some("\u{2733} Fix the build"));
        h.backends.tmux.pids.borrow_mut().insert("%3".into(), vec![30]);
        h.backends.tmux.add_command_pane("%4", "zsh", Some("\u{2733} wrapped"));
        h.backends.tmux.claude_screens.borrow_mut().push("%4".into());
        h.backends.tmux.add_command_pane("%5", "zsh", Some("plain"));
        h.backends.tmux.add_command_pane("%6", "vim", Some("vim"));
        let rows = list_rows(&h.ctx()).unwrap();
        let got: Vec<(&str, &str, BackendKind)> =
            rows.iter().map(|r| (r.name.as_str(), r.pane_id.as_str(), r.backend)).collect();
        assert_eq!(
            got,
            [("tmux-claude", "%3", BackendKind::Tmux), ("wrapped", "%4", BackendKind::Tmux)]
        );
        assert_eq!(rows[0].status, "\u{2013}");
        // and it can be targeted
        send(&h.ctx(), "hi", "wrapped", false, None).unwrap();
        assert_eq!(h.backends.tmux.sends().len(), 1);
    }

    #[test]
    fn unreachable_backend_skipped_silently() {
        let (h, _home) = harness(&[]);
        h.backends.herdr.fail_list.set(true);
        h.backends.tmux.panes.borrow_mut().clear();
        h.backends.tmux.add_command_pane("%3", "claude", Some("solo"));
        let rows = list_rows(&h.ctx()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "solo");
        h.backends.tmux.fail_list.set(true);
        assert!(list_rows(&h.ctx()).unwrap().is_empty());
        assert!(resolve(&h.ctx(), "solo", &h.names()).is_err());
    }
}
