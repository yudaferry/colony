//! `colony spawn` and `colony split`.

use super::log;
use crate::backend::{Direction, Kind, PaneRef, detect_env};
use crate::ctx::Ctx;
use crate::envelope;
use crate::state::log::LogEvent;
use crate::state::names::{Entry, NAMES_FILE, Names};
use anyhow::{Result, bail};
use std::path::PathBuf;

pub struct SpawnArgs {
    pub cmd: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub bypass: bool,
    /// `None` = default layout (50% right column, stacked equal); `Some` = plain split.
    pub direction: Option<Direction>,
}

impl SpawnArgs {
    /// A plain shell pane in the default layout.
    pub fn shell() -> Self {
        Self {
            cmd: None,
            name: None,
            cwd: None,
            bypass: false,
            direction: None,
        }
    }
}

pub fn split(ctx: &Ctx) -> Result<String> {
    spawn(ctx, &SpawnArgs::shell())
}

pub fn spawn(ctx: &Ctx, a: &SpawnArgs) -> Result<String> {
    let backend_kind = detect_env(ctx.env)?;
    let backend = ctx.backends.get(backend_kind);
    let is_claude = a.cmd.as_deref() == Some("claude");
    let name = a.name.as_ref().map(|n| n.to_lowercase());
    if is_claude && name.is_none() {
        bail!("spawn claude needs a name");
    }

    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    if let Some(n) = &name
        && let Some(e) = names.get(n)
    {
        // A stale entry (pane gone) may be overwritten; if the query fails, assume live.
        let live = match ctx.backends.get(e.backend).list_panes() {
            Ok(ps) => ps.iter().any(|p| p.pane_id == e.pane_id),
            Err(_) => true,
        };
        if live {
            bail!(
                "name '{n}' is already in use by pane {}; close it first or pick another name",
                e.pane_id
            );
        }
    }

    let from = backend.current_pane()?;
    let spawner_name = names.name_of_pane(backend_kind, &from).map(str::to_string);
    let spawner = spawner_name.clone().unwrap_or_else(|| "user".to_string());
    // Unnamed spawner: reply to its raw pane id only when that pane is detected as
    // Claude; a raw id to a shell would type the reply into it plus Enter.
    // A named spawner qualifies only if registered as Claude (a named shell would run it).
    let reply_to: Option<String> = match &spawner_name {
        Some(n) => names.get(n).is_some_and(|e| e.kind == Kind::Claude).then(|| n.clone()),
        None => (!from.is_empty() && super::register::is_claude_pane(backend, &from))
            .then(|| from.clone()),
    };
    let new_pane = match a.direction {
        Some(d) => backend.split(&from, d, a.cwd.as_deref())?,
        None => {
            // Live panes this pane spawned earlier form the right column, top to bottom.
            let live: Vec<String> = backend
                .list_panes()
                .map(|ps| ps.into_iter().map(|p| p.pane_id).collect())
                .unwrap_or_default();
            let column: Vec<String> = names
                .spawned_by(backend_kind, &from)
                .into_iter()
                .filter(|id| live.contains(id))
                .collect();
            backend.split_column(&from, &column, a.cwd.as_deref())?
        }
    };

    let kind = match a.cmd.as_deref() {
        None => Kind::Shell,
        Some("claude") => {
            let n = name.as_deref().unwrap_or_default();
            // An error here (e.g. the folder-trust prompt) is surfaced unchanged
            // and nothing is recorded.
            backend.start_claude(&new_pane, n, a.bypass)?;
            let target = PaneRef {
                backend: backend_kind,
                pane_id: new_pane.clone(),
                name: Some(n.to_string()),
                kind: Kind::Claude,
            };
            backend.send(&target, &envelope::intro(n, &new_pane, &spawner, reply_to.as_deref()))?;
            Kind::Claude
        }
        Some(c) => {
            backend.run_program(&new_pane, c)?;
            Kind::Other
        }
    };

    if let Some(n) = &name {
        let entry = Entry {
            backend: backend_kind,
            pane_id: new_pane.clone(),
            kind,
            cwd: a.cwd.as_ref().map(|p| p.display().to_string()),
            created_at: ctx.clock.now(),
            spawned_by: (!from.is_empty()).then(|| from.clone()),
        };
        ctx.state.update_json(NAMES_FILE, 0o644, |m: &mut Names| {
            m.insert(n, entry);
            Ok(())
        })?;
    }

    log(
        ctx,
        &LogEvent::Spawn {
            name: name.clone(),
            pane: new_pane.clone(),
            cmd: a.cmd.clone().unwrap_or_default(),
        },
    );

    Ok(match name {
        Some(n) => format!("{n} \u{2192} {new_pane}"),
        None => new_pane,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendKind;
    use crate::commands::testutil::Harness;

    fn args(cmd: Option<&str>, name: Option<&str>) -> SpawnArgs {
        SpawnArgs {
            cmd: cmd.map(String::from),
            name: name.map(String::from),
            ..SpawnArgs::shell()
        }
    }

    #[test]
    fn spawn_no_cmd_opens_shell_pane() {
        let h = Harness::herdr();
        let out = spawn(&h.ctx(), &SpawnArgs::shell()).unwrap();
        assert_eq!(out, "w1:p100");
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(log[0], "split_column w1:p1 right 50% ");
        assert!(h.names().0.is_empty());
    }

    #[test]
    fn split_equals_spawn_no_cmd() {
        let a = Harness::herdr();
        let b = Harness::herdr();
        let out_a = split(&a.ctx()).unwrap();
        let out_b = spawn(&b.ctx(), &SpawnArgs::shell()).unwrap();
        assert_eq!(out_a, out_b);
        assert_eq!(*a.backends.herdr.log.borrow(), *b.backends.herdr.log.borrow());
    }

    #[test]
    fn first_spawn_is_right_50() {
        let h = Harness::herdr();
        spawn(&h.ctx(), &SpawnArgs::shell()).unwrap();
        assert_eq!(h.backends.herdr.log.borrow()[0], "split_column w1:p1 right 50% ");
    }

    fn named(n: &str) -> SpawnArgs {
        args(Some("opencode"), Some(n))
    }

    #[test]
    fn second_spawn_is_below_first_equal_halves() {
        let h = Harness::herdr();
        spawn(&h.ctx(), &named("a")).unwrap();
        spawn(&h.ctx(), &named("b")).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[2], "split_column w1:p1 below w1:p100 equal=2 ");
        assert_eq!(h.names().get("b").unwrap().spawned_by.as_deref(), Some("w1:p1"));
    }

    #[test]
    fn third_spawn_equal_thirds_stacked_below_last() {
        let h = Harness::herdr();
        for n in ["a", "b", "c"] {
            spawn(&h.ctx(), &named(n)).unwrap();
        }
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[4], "split_column w1:p1 below w1:p101 equal=3 ");
    }

    #[test]
    fn dead_column_panes_are_ignored() {
        let h = Harness::herdr();
        spawn(&h.ctx(), &named("a")).unwrap();
        spawn(&h.ctx(), &named("b")).unwrap();
        h.backends.herdr.panes.borrow_mut().retain(|p| p.pane_id != "w1:p101");
        spawn(&h.ctx(), &named("c")).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[4], "split_column w1:p1 below w1:p100 equal=2 ");
        // all gone: start again with the 50% right split
        h.backends.herdr.panes.borrow_mut().retain(|p| p.pane_id == "w1:p1");
        spawn(&h.ctx(), &named("d")).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[6], "split_column w1:p1 right 50% ");
    }

    #[test]
    fn column_belongs_to_the_spawner_pane() {
        let h = Harness::herdr();
        spawn(&h.ctx(), &named("a")).unwrap();
        *h.backends.herdr.current.borrow_mut() = "w1:p100".into();
        spawn(&h.ctx(), &named("b")).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[2], "split_column w1:p100 right 50% ");
    }

    #[test]
    fn tmux_uses_same_layout() {
        let h = Harness::new(&[("TMUX", "/tmp/x,1,0")]);
        spawn(&h.ctx(), &named("a")).unwrap();
        spawn(&h.ctx(), &named("b")).unwrap();
        let log = h.backends.tmux.log.borrow().clone();
        assert_eq!(log[0], "split_column %1 right 50% ");
        assert_eq!(log[2], "split_column %1 below %100 equal=2 ");
    }

    #[test]
    fn direction_down() {
        let h = Harness::herdr();
        let mut a = SpawnArgs::shell();
        a.direction = Some(Direction::Down);
        spawn(&h.ctx(), &a).unwrap();
        assert!(h.backends.herdr.log.borrow()[0].contains(" Down"));
        assert!(h.names().0.is_empty());
    }

    #[test]
    fn spawn_claude_bypass_herdr() {
        let h = Harness::herdr();
        h.add_name("team-leader", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let mut a = args(Some("claude"), Some("bo-server"));
        a.cwd = Some(PathBuf::from("/x"));
        a.bypass = true;
        spawn(&h.ctx(), &a).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[0], "split_column w1:p1 right 50% /x");
        assert_eq!(log[1], "start_claude w1:p100 bo-server bypass=true");
        let want = format!(
            "send w1:p100 {}",
            envelope::intro("bo-server", "w1:p100", "team-leader", Some("team-leader"))
        );
        assert_eq!(log[2], want);
        let e = h.names().get("bo-server").unwrap().clone();
        assert_eq!(e.kind, Kind::Claude);
        assert_eq!(e.pane_id, "w1:p100");
        assert_eq!(e.cwd.as_deref(), Some("/x"));
    }

    #[test]
    fn unnamed_shell_spawner_gets_no_reply_target() {
        let h = Harness::herdr();
        h.backends.herdr.commands.borrow_mut().insert("w1:p1".into(), "zsh".into());
        spawn(&h.ctx(), &args(Some("claude"), Some("bo"))).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[2], format!("send w1:p100 {}", envelope::intro("bo", "w1:p100", "user", None)));
        assert!(!log[2].contains("colony send"), "{log:?}");
    }

    #[test]
    fn named_shell_spawner_gets_intro_without_reply() {
        let h = Harness::herdr();
        h.add_name("me", BackendKind::Herdr, "w1:p1", Kind::Shell);
        spawn(&h.ctx(), &args(Some("claude"), Some("bo"))).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[2], format!("send w1:p100 {}", envelope::intro("bo", "w1:p100", "me", None)));
        assert!(!log[2].contains("colony send"), "{log:?}");
    }

    #[test]
    fn unnamed_claude_spawner_gets_raw_id_reply_target() {
        let h = Harness::herdr();
        h.backends.herdr.commands.borrow_mut().insert("w1:p1".into(), "claude".into());
        spawn(&h.ctx(), &args(Some("claude"), Some("bo"))).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(
            log[2],
            format!("send w1:p100 {}", envelope::intro("bo", "w1:p100", "user", Some("w1:p1")))
        );
    }

    #[test]
    fn spawn_opencode_runs_program() {
        let h = Harness::herdr();
        spawn(&h.ctx(), &args(Some("opencode"), Some("oc"))).unwrap();
        let log = h.backends.herdr.log.borrow().clone();
        assert_eq!(log[1], "run w1:p100 opencode");
        assert_eq!(h.names().get("oc").unwrap().kind, Kind::Other);
    }

    #[test]
    fn spawn_non_claude_with_empty_run_output_records_name() {
        use crate::backend::{Backend, BackendSet, FakeRunner, Herdr};
        use crate::backend::runner::sv;
        struct Set(Herdr<FakeRunner>);
        impl BackendSet for Set {
            fn get(&self, _k: BackendKind) -> &dyn Backend {
                &self.0
            }
        }
        let h = Harness::herdr();
        let set = Set(Herdr::new(FakeRunner::new(), Some("w1:p1".into())));
        set.0.runner().push_ok(r#"{"result":{"agents":[]}}"#); // caller pane check
        set.0.runner().push_ok(r#"{"result":{"panes":[{"pane_id":"w1:p1"}]}}"#); // live panes
        set.0.runner().push_ok(r#"{"result":{"agents":[]}}"#);
        set.0.runner().push_ok(r#"{"result":{"pane":{"pane_id":"w1:p9"}}}"#);
        set.0.runner().push_ok(""); // pane run prints nothing
        let mut ctx = h.ctx();
        ctx.backends = &set;
        spawn(&ctx, &args(Some("zsh"), Some("t-sh"))).unwrap();
        let calls = set.0.runner().calls();
        assert_eq!(calls[4], sv(&["herdr", "pane", "run", "w1:p9", "zsh"]));
        assert_eq!(h.names().get("t-sh").unwrap().pane_id, "w1:p9");
    }

    #[test]
    fn spawn_claude_without_name_errors() {
        let h = Harness::herdr();
        let msg = spawn(&h.ctx(), &args(Some("claude"), None)).unwrap_err().to_string();
        assert!(msg.contains("spawn claude needs a name"), "{msg}");
        assert!(h.backends.herdr.log.borrow().is_empty());
    }

    #[test]
    fn spawn_existing_live_name_errors() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let msg = spawn(&h.ctx(), &args(Some("claude"), Some("BO"))).unwrap_err().to_string();
        assert!(msg.contains("already in use"), "{msg}");
        assert!(h.backends.herdr.log.borrow().is_empty());
    }

    #[test]
    fn spawn_output_per_d8() {
        let h = Harness::herdr();
        let named = spawn(&h.ctx(), &args(Some("opencode"), Some("oc"))).unwrap();
        assert_eq!(named, "oc \u{2192} w1:p100");
        let unnamed = spawn(&h.ctx(), &SpawnArgs::shell()).unwrap();
        assert_eq!(unnamed, "w1:p101");
    }

    #[test]
    fn spawn_logged() {
        let h = Harness::herdr();
        spawn(&h.ctx(), &args(Some("opencode"), Some("oc"))).unwrap();
        let log = h.log_text();
        assert!(log.contains("SPAWN name=oc pane=w1:p100 cmd=opencode"), "{log}");
    }

    #[test]
    fn spawn_no_env_errors() {
        let h = Harness::new(&[]);
        assert!(spawn(&h.ctx(), &SpawnArgs::shell()).is_err());
    }
}
