//! One function per CLI command; each returns the stdout text.

pub mod close;
pub mod list;
pub mod read;
pub mod register;
pub mod send;
pub mod spawn;
pub mod wait;

use crate::backend::{AgentState, BackendKind, Kind};
use crate::ctx::Ctx;
use crate::state::log::{LogEvent, log_event, log_path};

/// Appends a log line; a logging failure is a warning, never a command failure.
pub(crate) fn log(ctx: &Ctx, ev: &LogEvent) {
    let path = log_path(ctx.state, ctx.env);
    if let Err(e) = log_event(&path, ctx.clock.now(), ev) {
        eprintln!("colony: warning: could not write log: {e:#}");
    }
}

pub(crate) fn backend_str(b: BackendKind) -> &'static str {
    match b {
        BackendKind::Herdr => "herdr",
        BackendKind::Tmux => "tmux",
    }
}

pub(crate) fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Claude => "claude",
        Kind::Shell => "shell",
        Kind::Other => "other",
    }
}

pub(crate) fn state_str(s: AgentState) -> &'static str {
    match s {
        AgentState::Idle => "idle",
        AgentState::Working => "working",
        AgentState::Blocked => "blocked",
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use crate::backend::{BackendKind, FakeBackends, Kind};
    use crate::ctx::{Ctx, FixedClock, MapEnv};
    use crate::state::names::{Entry, NAMES_FILE, Names};
    use crate::state::store::StateDir;
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;

    /// Everything a command test needs: temp state dir, fake backends, fake env and clock.
    pub struct Harness {
        pub _dir: tempfile::TempDir,
        pub state: StateDir,
        pub backends: FakeBackends,
        pub env: MapEnv,
        pub clock: FixedClock,
    }

    impl Harness {
        pub fn new(env: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let state = StateDir::new(dir.path().to_path_buf());
            let env = MapEnv(
                env.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
            );
            Self {
                _dir: dir,
                state,
                backends: FakeBackends::new(),
                env,
                clock: FixedClock(Utc.with_ymd_and_hms(2026, 10, 29, 14, 52, 3).unwrap()),
            }
        }

        /// Harness whose caller runs inside herdr pane `w1:p1`.
        pub fn herdr() -> Self {
            Self::new(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w1:p1")])
        }

        pub fn ctx(&self) -> Ctx<'_> {
            Ctx {
                state: &self.state,
                backends: &self.backends,
                env: &self.env,
                clock: &self.clock,
            }
        }

        pub fn add_name(&self, name: &str, backend: BackendKind, pane: &str, kind: Kind) {
            let entry = Entry {
                backend,
                pane_id: pane.to_string(),
                kind,
                cwd: None,
                created_at: self.clock.0,
                spawned_by: None,
            };
            self.state
                .update_json(NAMES_FILE, 0o644, |n: &mut Names| {
                    n.insert(name, entry);
                    Ok(())
                })
                .unwrap();
        }

        pub fn names(&self) -> Names {
            self.state.read_json(NAMES_FILE).unwrap()
        }

        pub fn log_text(&self) -> String {
            std::fs::read_to_string(self.state.path("colony.log")).unwrap_or_default()
        }
    }
}
