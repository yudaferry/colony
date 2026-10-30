//! Backends: herdr and tmux behind one trait, plus fakes for tests.

pub mod fake;
pub mod herdr;
pub mod runner;
pub mod tmux;

use crate::ctx::Env;
pub use crate::state::names::{BackendKind, Kind};
use anyhow::{Result, anyhow};
use clap::ValueEnum;
use std::path::Path;
use std::time::Duration;

pub use fake::FakeBackend;
pub use herdr::Herdr;
pub use runner::{FakeRunner, ProcessRunner, Runner};
pub use tmux::Tmux;

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Direction {
    Right,
    Down,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaneRef {
    pub backend: BackendKind,
    pub pane_id: String,
    pub name: Option<String>,
    pub kind: Kind,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PaneInfo {
    pub pane_id: String,
    /// Agent kind (e.g. `claude`) and its state, when an agent runs in the pane.
    pub agent: Option<(String, AgentState)>,
    /// Terminal title (herdr: `terminal_title_stripped`; tmux: `#{pane_title}`),
    /// `None` when empty or just the host name. Claude sets it to the `/rename` name.
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// tmux: `#{pane_current_command}`, so discovery needs no second call.
    pub command: Option<String>,
    /// tmux: `#{pane_pid}` (the pane's first process).
    pub pid: Option<u32>,
}

#[derive(thiserror::Error, Debug)]
pub enum BackendError {
    #[error("herdr only")]
    HerdrOnly,
    #[error("{0}")]
    Other(String),
}

pub trait Backend {
    fn kind(&self) -> BackendKind;
    fn current_pane(&self) -> Result<String>;
    fn list_panes(&self) -> Result<Vec<PaneInfo>>;
    /// Returns the new pane id.
    fn split(&self, from_pane: &str, dir: Direction, cwd: Option<&Path>) -> Result<String>;
    /// Default spawn layout: the first pane of a column splits `lead` to the right at
    /// 50% width; later ones stack below the last column pane and the column is made
    /// equal in height. `column` holds the live column panes, top to bottom.
    /// Returns the new pane id.
    fn split_column(&self, lead: &str, column: &[String], cwd: Option<&Path>) -> Result<String>;
    fn run_program(&self, pane: &str, cmd: &str) -> Result<()>;
    fn start_claude(&self, pane: &str, name: &str, bypass: bool) -> Result<()>;
    /// tmux: no-op.
    fn name_agent(&self, pane: &str, name: &str) -> Result<()>;
    /// Text plus submit.
    fn send(&self, target: &PaneRef, text: &str) -> Result<()>;
    fn read(&self, target: &PaneRef, lines: usize) -> Result<String>;
    fn wait(&self, target: &PaneRef, until: AgentState, timeout: Duration) -> Result<()>;
    fn close(&self, pane: &str) -> Result<()>;
    /// tmux: `Ok(None)`.
    fn agent_status(&self, target: &PaneRef) -> Result<Option<AgentState>>;
    /// Foreground program of the pane, for D-4 kind detection.
    fn pane_command(&self, pane: &str) -> Result<Option<String>>;
    /// True when the pane screen shows Claude (used when the foreground program is a
    /// shell wrapper). Backends without a screen capture answer false.
    fn screen_is_claude(&self, _pane: &str) -> bool {
        false
    }
    /// PIDs of the processes in the pane that may be the Claude Code process; used to
    /// find `~/.claude/sessions/<pid>.json`. Best effort: errors and unknowns give `[]`.
    fn claude_pids(&self, _pane: &PaneInfo) -> Vec<u32> {
        Vec::new()
    }
}

/// True when a backend error says the pane is gone or the backend itself is not running
/// (tmux: "can't find pane", "no server running"; herdr: `pane_not_found`).
pub fn is_pane_gone(err: &anyhow::Error) -> bool {
    let low = format!("{err:#}").to_lowercase();
    [
        "can't find pane",
        "no server running",
        "error connecting to",
        "server exited unexpectedly",
        "pane_not_found",
    ]
    .iter()
    .any(|m| low.contains(m))
}

/// Which backend a command that creates or names a pane should use (DESIGN 4.1).
pub fn detect_env(env: &dyn Env) -> Result<BackendKind> {
    let set = |key: &str| env.var(key).is_some_and(|v| !v.is_empty());
    // tmux is the innermost multiplexer: a tmux started from a herdr pane inherits HERDR_*.
    if set("TMUX") || set("TMUX_PANE") {
        return Ok(BackendKind::Tmux);
    }
    if env.var("HERDR_ENV").as_deref() == Some("1") || set("HERDR_PANE_ID") {
        return Ok(BackendKind::Herdr);
    }
    Err(anyhow!(
        "not inside herdr or tmux: none of HERDR_ENV, HERDR_PANE_ID or TMUX is set"
    ))
}

/// One process drives both backends; a command picks one by `BackendKind`.
pub trait BackendSet {
    fn get(&self, kind: BackendKind) -> &dyn Backend;
}

pub struct RealBackends {
    herdr: Herdr<ProcessRunner>,
    tmux: Tmux<ProcessRunner>,
}

impl RealBackends {
    /// Both backends, built from the process environment
    /// (`HERDR_PANE_ID` and `TMUX_PANE` give the current pane).
    pub fn from_env(env: &dyn Env) -> Self {
        Self {
            herdr: Herdr::new(ProcessRunner, env.var("HERDR_PANE_ID").filter(|v| !v.is_empty())),
            tmux: Tmux::new(ProcessRunner, env.var("TMUX_PANE").filter(|v| !v.is_empty())),
        }
    }
}

impl BackendSet for RealBackends {
    fn get(&self, kind: BackendKind) -> &dyn Backend {
        match kind {
            BackendKind::Herdr => &self.herdr,
            BackendKind::Tmux => &self.tmux,
        }
    }
}

pub struct FakeBackends {
    pub herdr: FakeBackend,
    pub tmux: FakeBackend,
}

impl FakeBackends {
    pub fn new() -> Self {
        Self {
            herdr: FakeBackend::new(BackendKind::Herdr),
            tmux: FakeBackend::new(BackendKind::Tmux),
        }
    }
}

impl Default for FakeBackends {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendSet for FakeBackends {
    fn get(&self, kind: BackendKind) -> &dyn Backend {
        match kind {
            BackendKind::Herdr => &self.herdr,
            BackendKind::Tmux => &self.tmux,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::MapEnv;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        )
    }

    #[test]
    fn detect_herdr_env() {
        assert_eq!(detect_env(&env(&[("HERDR_ENV", "1")])).unwrap(), BackendKind::Herdr);
    }

    #[test]
    fn detect_herdr_pane_id() {
        assert_eq!(
            detect_env(&env(&[("HERDR_PANE_ID", "w1:p1")])).unwrap(),
            BackendKind::Herdr
        );
    }

    #[test]
    fn detect_tmux() {
        assert_eq!(
            detect_env(&env(&[("TMUX", "/tmp/tmux-1000/default,1,0")])).unwrap(),
            BackendKind::Tmux
        );
    }

    #[test]
    fn tmux_wins_when_both_set() {
        let e = env(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w8:pZ"), ("TMUX", "/tmp/x,1,0"), ("TMUX_PANE", "%0")]);
        assert_eq!(detect_env(&e).unwrap(), BackendKind::Tmux);
    }

    #[test]
    fn pane_gone_errors_recognised() {
        for m in [
            "tmux kill-pane -t failed (exit status: 1): can't find pane: %0",
            "tmux kill-pane failed: no server running on /tmp/tmux-1000/default",
            "error connecting to /tmp/tmux-1000/default (No such file or directory)",
            "herdr: pane_not_found",
        ] {
            assert!(is_pane_gone(&anyhow!(m.to_string())), "{m}");
        }
        assert!(!is_pane_gone(&anyhow!("permission denied")));
    }

    #[test]
    fn detect_none_errors_naming_vars() {
        let msg = detect_env(&env(&[])).unwrap_err().to_string();
        assert!(msg.contains("HERDR_ENV"), "{msg}");
        assert!(msg.contains("HERDR_PANE_ID"), "{msg}");
        assert!(msg.contains("TMUX"), "{msg}");
    }

    #[test]
    fn one_call_reaches_both_backends() {
        let set = FakeBackends::new();
        let h = PaneRef {
            backend: BackendKind::Herdr,
            pane_id: "w1:p1".into(),
            name: Some("a".into()),
            kind: Kind::Claude,
        };
        let t = PaneRef {
            backend: BackendKind::Tmux,
            pane_id: "%1".into(),
            name: Some("b".into()),
            kind: Kind::Claude,
        };
        set.get(h.backend).send(&h, "to-herdr").unwrap();
        assert_eq!(set.herdr.sends().len(), 1);
        assert!(set.tmux.sends().is_empty());
        set.get(t.backend).send(&t, "to-tmux").unwrap();
        assert_eq!(set.herdr.sends().len(), 1);
        assert_eq!(set.tmux.sends().len(), 1);
    }
}
