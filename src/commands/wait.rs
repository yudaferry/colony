//! `colony wait` (herdr only).

use super::state_str;
use crate::backend::AgentState;
use crate::ctx::Ctx;
use crate::state::names::{NAMES_FILE, Names};
use crate::discover::resolve;
use anyhow::Result;
use std::time::Duration;

pub fn wait(ctx: &Ctx, target: &str, until: AgentState, timeout: Duration) -> Result<String> {
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let t = resolve(ctx, target, &names)?;
    ctx.backends.get(t.backend).wait(&t, until, timeout)?;
    Ok(format!("{target} is {}", state_str(until)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendKind, Kind};
    use crate::commands::testutil::Harness;

    #[test]
    fn wait_herdr_ok() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        let out = wait(&h.ctx(), "bo", AgentState::Blocked, Duration::from_secs(30)).unwrap();
        assert_eq!(out, "bo is blocked");
        assert!(
            h.backends
                .herdr
                .log
                .borrow()
                .contains(&"wait w1:p2 Blocked 30s".to_string())
        );
    }

    #[test]
    fn wait_herdr_timeout_errors() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        h.backends.herdr.fail_wait.set(true);
        let err = wait(&h.ctx(), "bo", AgentState::Idle, Duration::from_secs(1)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[test]
    fn wait_tmux_prints_herdr_only() {
        let h = Harness::herdr();
        h.add_name("tw", BackendKind::Tmux, "%1", Kind::Claude);
        let err = wait(&h.ctx(), "tw", AgentState::Idle, Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.to_string(), "herdr only");
    }
}
