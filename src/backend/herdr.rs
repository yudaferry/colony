//! herdr backend. Commands and JSON paths come from the live verification
//! recorded in plan/TASK.md ("Verification results", herdr 0.9.5).
//!
//! Free text is always passed as ONE argv element and WITHOUT `--`: verified
//! live that `herdr agent prompt` treats `--` as text (and then rejects the real
//! text), while text starting with `-` is accepted as it is.

use super::runner::sv;
use super::{
    AgentState, Backend, BackendError, BackendKind, Direction, Kind, PaneInfo, PaneRef, Runner,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

const HERDR: &str = "herdr";

pub struct Herdr<R: Runner> {
    runner: R,
    env_pane: Option<String>,
}

impl<R: Runner> Herdr<R> {
    pub fn new(runner: R, env_pane: Option<String>) -> Self {
        Self { runner, env_pane }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    fn raw(&self, args: &[&str]) -> Result<String> {
        self.runner.run(HERDR, &sv(args))
    }

    /// Runs a herdr command whose stdout is JSON and returns the parsed value.
    fn json(&self, args: &[&str]) -> Result<Value> {
        let out = self.raw(args)?;
        parse_json(&out, args)
    }

    /// Runs a herdr command whose success output may be empty or non-JSON.
    fn ok(&self, args: &[&str]) -> Result<()> {
        check_lenient(&self.raw(args)?)
    }

    fn agents(&self) -> Result<Vec<Value>> {
        let v = self.json(&["agent", "list"])?;
        Ok(v.pointer("/result/agents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    fn split_with(
        &self,
        from_pane: &str,
        dir: Direction,
        ratio: Option<f64>,
        cwd: Option<&Path>,
    ) -> Result<String> {
        let dir = match dir {
            Direction::Right => "right",
            Direction::Down => "down",
        };
        let cwd = cwd.map(|p| p.to_string_lossy().into_owned());
        let ratio = ratio.map(|r| format!("{r}"));
        let mut args = vec!["pane", "split", "--pane", from_pane, "--direction", dir];
        if let Some(r) = ratio.as_deref() {
            args.push("--ratio");
            args.push(r);
        }
        if let Some(c) = cwd.as_deref() {
            args.push("--cwd");
            args.push(c);
        }
        args.push("--no-focus");
        let v = self.json(&args)?;
        v.pointer("/result/pane/pane_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("herdr pane split: missing result.pane.pane_id"))
    }

    /// Makes the stacked `column` panes (top to bottom) equal in height.
    /// herdr has no equalize command. The column is a chain of nested splits: the
    /// divider below pane i has ratio h_i / (h_i + ... + h_last) and must be 1/(m-i).
    /// `pane resize --pane P --direction up|down --amount D` moves the divider above
    /// (up) or below (down) P by D of that split's ratio (verified live).
    fn equalise_heights(&self, column: &[&str]) -> Result<()> {
        let m = column.len();
        let layout = self.json(&["pane", "layout", "--pane", column[m - 1]])?;
        let panes = layout
            .pointer("/result/layout/panes")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("herdr pane layout: missing result.layout.panes"))?;
        let height = |id: &str| -> Option<f64> {
            panes
                .iter()
                .find(|p| p.get("pane_id").and_then(Value::as_str) == Some(id))
                .and_then(|p| p.pointer("/rect/height"))
                .and_then(Value::as_f64)
        };
        let heights: Vec<f64> = column.iter().map(|id| height(id)).collect::<Option<_>>()
            .ok_or_else(|| anyhow!("herdr pane layout: column pane missing"))?;
        for i in 0..m - 1 {
            let rest: f64 = heights[i..].iter().sum();
            let delta = heights[i] / rest - 1.0 / (m - i) as f64;
            if (delta * rest).abs() < 1.0 {
                continue; // under one row
            }
            let amount = format!("{:.4}", delta.abs());
            // Too tall: pull the divider below pane i up (the top divider of pane i+1).
            // Too short: push the divider below pane i down.
            let (pane, dir) = if delta > 0.0 { (column[i + 1], "up") } else { (column[i], "down") };
            self.ok(&["pane", "resize", "--pane", pane, "--direction", dir, "--amount", amount.as_str()])?;
        }
        Ok(())
    }

    /// herdr agent name: the colony name for named panes, else the pane id.
    fn agent_target<'a>(target: &'a PaneRef) -> &'a str {
        target.name.as_deref().unwrap_or(&target.pane_id)
    }
}

/// Success output of these commands may be empty or non-JSON (`pane run` prints
/// nothing, verified live). Exit status 0 already means success; only a JSON
/// error envelope in the output is still reported.
fn check_lenient(out: &str) -> Result<()> {
    if let Ok(v) = serde_json::from_str::<Value>(out.trim()) {
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
            bail!("herdr error {code}: {msg}");
        }
    }
    Ok(())
}

fn parse_json(out: &str, args: &[&str]) -> Result<Value> {
    let v: Value = serde_json::from_str(out.trim()).with_context(|| {
        format!(
            "herdr {} returned output that is not JSON",
            args.iter().take(2).copied().collect::<Vec<_>>().join(" ")
        )
    })?;
    if let Some(err) = v.get("error") {
        let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
        bail!("herdr error {code}: {msg}");
    }
    Ok(v)
}

/// herdr `agent_status` -> colony state. `done` (finished, not yet viewed)
/// counts as idle; `unknown` (startup) counts as working so waiters keep waiting.
fn parse_state(s: &str) -> AgentState {
    match s {
        "idle" | "done" => AgentState::Idle,
        "blocked" => AgentState::Blocked,
        _ => AgentState::Working,
    }
}

fn state_arg(s: AgentState) -> &'static str {
    match s {
        AgentState::Idle => "idle",
        AgentState::Working => "working",
        AgentState::Blocked => "blocked",
    }
}

impl<R: Runner> Backend for Herdr<R> {
    fn kind(&self) -> BackendKind {
        BackendKind::Herdr
    }

    fn current_pane(&self) -> Result<String> {
        self.env_pane
            .clone()
            .ok_or_else(|| anyhow!("HERDR_PANE_ID is not set; not running inside a herdr pane"))
    }

    fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        let panes = self.json(&["pane", "list"])?;
        let agents = self.agents()?;
        let ids = panes
            .pointer("/result/panes")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("herdr pane list: missing result.panes"))?;
        Ok(ids
            .iter()
            .filter_map(|p| p.get("pane_id").and_then(Value::as_str))
            .map(|id| {
                let agent = agents
                    .iter()
                    .find(|a| a.get("pane_id").and_then(Value::as_str) == Some(id))
                    .map(|a| {
                        let kind = a.get("agent").and_then(Value::as_str).unwrap_or("").to_string();
                        let st = a.get("agent_status").and_then(Value::as_str).unwrap_or("unknown");
                        (kind, parse_state(st))
                    });
                PaneInfo {
                    pane_id: id.to_string(),
                    agent,
                }
            })
            .collect())
    }

    fn split(&self, from_pane: &str, dir: Direction, cwd: Option<&Path>) -> Result<String> {
        self.split_with(from_pane, dir, None, cwd)
    }

    fn split_column(&self, lead: &str, column: &[String], cwd: Option<&Path>) -> Result<String> {
        let Some(last) = column.last() else {
            // --ratio is the share kept by the OLD pane (verified live): 0.5 = 50% each.
            return self.split_with(lead, Direction::Right, Some(0.5), cwd);
        };
        let new = self.split_with(last, Direction::Down, Some(0.5), cwd)?;
        let mut all: Vec<&str> = column.iter().map(String::as_str).collect();
        all.push(new.as_str());
        // Best effort: the pane exists already.
        let _ = self.equalise_heights(&all);
        Ok(new)
    }

    fn run_program(&self, pane: &str, cmd: &str) -> Result<()> {
        self.ok(&["pane", "run", pane, cmd])
    }

    fn start_claude(&self, pane: &str, name: &str, bypass: bool) -> Result<()> {
        let mut args = vec![
            "agent", "start", name, "--kind", "claude", "--pane", pane, "--", "-n", name,
        ];
        if bypass {
            args.push("--dangerously-skip-permissions");
        }
        match self.json(&args) {
            Ok(_) => Ok(()),
            Err(e) if format!("{e:#}").contains("agent_not_ready") => Err(anyhow!(
                "Claude in pane {pane} is waiting for you to accept the folder-trust prompt; \
                 accept it there, then run: colony register {name} {pane}"
            )),
            Err(e) => Err(e),
        }
    }

    fn name_agent(&self, pane: &str, name: &str) -> Result<()> {
        self.ok(&["agent", "rename", pane, name])
    }

    fn send(&self, target: &PaneRef, text: &str) -> Result<()> {
        if target.kind == Kind::Claude {
            self.json(&["agent", "prompt", Self::agent_target(target), text])
                .map(|_| ())
        } else {
            self.ok(&["pane", "run", target.pane_id.as_str(), text])
        }
    }

    fn read(&self, target: &PaneRef, lines: usize) -> Result<String> {
        let n = lines.to_string();
        if target.kind == Kind::Claude {
            self.raw(&[
                "agent",
                "read",
                Self::agent_target(target),
                "--source",
                "recent-unwrapped",
                "--lines",
                n.as_str(),
            ])
        } else {
            self.raw(&[
                "pane",
                "read",
                target.pane_id.as_str(),
                "--source",
                "recent-unwrapped",
                "--lines",
                n.as_str(),
            ])
        }
    }

    fn wait(&self, target: &PaneRef, until: AgentState, timeout: Duration) -> Result<()> {
        if target.kind != Kind::Claude {
            return Err(BackendError::Other(format!(
                "{} is not a Claude agent pane; wait needs an agent",
                target.pane_id
            ))
            .into());
        }
        let ms = timeout.as_millis().to_string();
        let mut args = vec!["agent", "wait", Self::agent_target(target), "--until", state_arg(until)];
        if until == AgentState::Idle {
            // herdr reports a finished, unviewed turn as `done`; it also counts as idle.
            args.push("--until");
            args.push("done");
        }
        args.push("--timeout");
        args.push(&ms);
        self.json(&args).map(|_| ())
    }

    fn close(&self, pane: &str) -> Result<()> {
        self.ok(&["pane", "close", pane])
    }

    fn agent_status(&self, target: &PaneRef) -> Result<Option<AgentState>> {
        Ok(self
            .agents()?
            .iter()
            .find(|a| a.get("pane_id").and_then(Value::as_str) == Some(target.pane_id.as_str()))
            .map(|a| parse_state(a.get("agent_status").and_then(Value::as_str).unwrap_or("unknown"))))
    }

    /// herdr has no process query; the detected agent kind (e.g. `claude`) is
    /// returned, `None` for panes without an agent.
    fn pane_command(&self, pane: &str) -> Result<Option<String>> {
        Ok(self
            .agents()?
            .iter()
            .find(|a| a.get("pane_id").and_then(Value::as_str) == Some(pane))
            .and_then(|a| a.get("agent").and_then(Value::as_str))
            .map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeRunner;
    use std::path::PathBuf;

    const PANE_SPLIT: &str = include_str!("../../tests/fixtures/herdr/pane_split.json");
    const PANE_LIST: &str = include_str!("../../tests/fixtures/herdr/pane_list.json");
    const AGENT_LIST: &str = include_str!("../../tests/fixtures/herdr/agent_list.json");
    const AGENT_READ: &str = include_str!("../../tests/fixtures/herdr/agent_read.txt");

    fn herdr() -> Herdr<FakeRunner> {
        Herdr::new(FakeRunner::new(), Some("w8:pZ".to_string()))
    }

    fn target(kind: Kind, name: Option<&str>, pane: &str) -> PaneRef {
        PaneRef {
            backend: BackendKind::Herdr,
            pane_id: pane.to_string(),
            name: name.map(str::to_string),
            kind,
        }
    }

    #[test]
    fn split_right_with_cwd_argv() {
        let h = herdr();
        h.runner().push_ok(PANE_SPLIT);
        let dir = PathBuf::from("/x/y");
        h.split("w8:pZ", Direction::Right, Some(&dir)).unwrap();
        assert_eq!(
            h.runner().calls(),
            vec![sv(&[
                "herdr", "pane", "split", "--pane", "w8:pZ", "--direction", "right", "--cwd",
                "/x/y", "--no-focus"
            ])]
        );
    }

    #[test]
    fn split_down_without_cwd_omits_flag() {
        let h = herdr();
        h.runner().push_ok(PANE_SPLIT);
        h.split("w8:pZ", Direction::Down, None).unwrap();
        let call = &h.runner().calls()[0];
        assert!(call.contains(&"down".to_string()));
        assert!(!call.contains(&"--cwd".to_string()));
    }

    #[test]
    fn split_parses_pane_id_from_fixture() {
        let h = herdr();
        h.runner().push_ok(PANE_SPLIT);
        assert_eq!(h.split("w8:pZ", Direction::Right, None).unwrap(), "w8:p11");
    }

    #[test]
    fn list_parses_fixture() {
        let h = herdr();
        h.runner().push_ok(PANE_LIST);
        h.runner().push_ok(AGENT_LIST);
        let panes = h.list_panes().unwrap();
        assert_eq!(
            h.runner().calls(),
            vec![sv(&["herdr", "pane", "list"]), sv(&["herdr", "agent", "list"])]
        );
        assert_eq!(panes.len(), 14);
        let find = |id: &str| panes.iter().find(|p| p.pane_id == id).unwrap();
        assert_eq!(find("w8:pZ").agent, Some(("claude".to_string(), AgentState::Working)));
        assert_eq!(find("wB:pN").agent, Some(("claude".to_string(), AgentState::Idle)));
        assert_eq!(find("w8:p11").agent, None);
    }

    #[test]
    fn agent_status_parses_fixture() {
        let h = herdr();
        h.runner().push_ok(AGENT_LIST);
        h.runner().push_ok(AGENT_LIST);
        h.runner().push_ok(AGENT_LIST);
        let t = |p| target(Kind::Claude, None, p);
        assert_eq!(h.agent_status(&t("wB:pN")).unwrap(), Some(AgentState::Idle));
        assert_eq!(h.agent_status(&t("w8:pZ")).unwrap(), Some(AgentState::Working));
        assert_eq!(h.agent_status(&t("w8:p11")).unwrap(), None);
    }

    #[test]
    fn start_claude_bypass_argv() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.start_claude("w8:p12", "bo", true).unwrap();
        assert_eq!(
            h.runner().calls(),
            vec![sv(&[
                "herdr", "agent", "start", "bo", "--kind", "claude", "--pane", "w8:p12", "--",
                "-n", "bo", "--dangerously-skip-permissions"
            ])]
        );
    }

    #[test]
    fn start_claude_no_bypass_omits_flag() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.start_claude("w8:p12", "bo", false).unwrap();
        let call = &h.runner().calls()[0];
        assert!(!call.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert_eq!(call.last().unwrap(), "bo");
    }

    #[test]
    fn start_claude_trust_prompt_gives_clear_error() {
        let h = herdr();
        h.runner().push_err(
            "herdr agent start failed (exit status: 1): \
             {\"error\":{\"code\":\"agent_not_ready\",\"message\":\"blocked during startup\"}}",
        );
        let msg = h.start_claude("w8:p12", "bo", false).unwrap_err().to_string();
        assert_eq!(
            msg,
            "Claude in pane w8:p12 is waiting for you to accept the folder-trust prompt; \
             accept it there, then run: colony register bo w8:p12"
        );
    }

    #[test]
    fn start_claude_other_errors_pass_through() {
        let h = herdr();
        h.runner().push_err("something else");
        let msg = h.start_claude("w8:p12", "bo", false).unwrap_err().to_string();
        assert!(msg.contains("something else"));
    }

    #[test]
    fn split_column_first_is_right_half_of_lead() {
        let h = herdr();
        h.runner().push_ok(PANE_SPLIT);
        h.split_column("w8:p1", &[], None).unwrap();
        assert_eq!(
            h.runner().calls(),
            vec![sv(&[
                "herdr", "pane", "split", "--pane", "w8:p1", "--direction", "right", "--ratio", "0.5",
                "--no-focus"
            ])]
        );
    }

    fn layout(heights: &[(&str, u32)]) -> String {
        let panes: Vec<String> = heights
            .iter()
            .map(|(id, h)| format!(r#"{{"pane_id":"{id}","rect":{{"height":{h}}}}}"#))
            .collect();
        format!(r#"{{"result":{{"layout":{{"panes":[{{"pane_id":"lead","rect":{{"height":45}}}},{}]}}}}}}"#, panes.join(","))
    }

    #[test]
    fn split_column_second_stacks_below_and_stays_equal() {
        let h = herdr();
        h.runner().push_ok(PANE_SPLIT);
        h.runner().push_ok(&layout(&[("a", 23), ("w8:p11", 22)]));
        let col = vec!["a".to_string()];
        h.split_column("lead", &col, None).unwrap();
        let calls = h.runner().calls();
        assert_eq!(calls.len(), 2, "{calls:?}"); // already equal: no resize
        assert_eq!(
            calls[0],
            sv(&["herdr", "pane", "split", "--pane", "a", "--direction", "down", "--ratio", "0.5", "--no-focus"])
        );
        assert_eq!(calls[1], sv(&["herdr", "pane", "layout", "--pane", "w8:p11"]));
    }

    #[test]
    fn split_column_third_resizes_to_thirds() {
        let h = herdr();
        h.runner().push_ok(PANE_SPLIT);
        // a=22, b=11, new=11 of 44: thirds need a to shrink 1/2 -> 1/3.
        h.runner().push_ok(&layout(&[("a", 22), ("b", 11), ("w8:p11", 11)]));
        let col = vec!["a".to_string(), "b".to_string()];
        h.split_column("lead", &col, None).unwrap();
        let calls = h.runner().calls();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(
            calls[2],
            sv(&["herdr", "pane", "resize", "--pane", "b", "--direction", "up", "--amount", "0.1667"])
        );
    }

    #[test]
    fn split_column_grows_short_pane_downwards() {
        let h = herdr();
        h.runner().push_ok(PANE_SPLIT);
        h.runner().push_ok(&layout(&[("a", 5), ("b", 20), ("w8:p11", 20)]));
        let col = vec!["a".to_string(), "b".to_string()];
        h.split_column("lead", &col, None).unwrap();
        let calls = h.runner().calls();
        assert_eq!(
            calls[2],
            sv(&["herdr", "pane", "resize", "--pane", "a", "--direction", "down", "--amount", "0.2222"])
        );
    }

    #[test]
    fn name_agent_argv() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.name_agent("w8:p12", "bo").unwrap();
        assert_eq!(h.runner().calls(), vec![sv(&["herdr", "agent", "rename", "w8:p12", "bo"])]);
    }

    #[test]
    fn send_claude_uses_agent_prompt() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.send(&target(Kind::Claude, Some("bo"), "w8:p12"), "hello").unwrap();
        assert_eq!(h.runner().calls(), vec![sv(&["herdr", "agent", "prompt", "bo", "hello"])]);
    }

    #[test]
    fn send_shell_uses_pane_commands_not_agent() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.send(&target(Kind::Shell, None, "w8:p12"), "ls -la").unwrap();
        let calls = h.runner().calls();
        assert_eq!(calls, vec![sv(&["herdr", "pane", "run", "w8:p12", "ls -la"])]);
        assert!(calls.iter().flatten().all(|a| a != "agent"));
    }

    #[test]
    fn pane_run_empty_stdout_is_ok() {
        let h = herdr();
        h.runner().push_ok("");
        h.run_program("w8:p12", "zsh").unwrap();
    }

    #[test]
    fn send_shell_empty_stdout_is_ok() {
        let h = herdr();
        h.runner().push_ok("");
        h.send(&target(Kind::Shell, None, "w8:p12"), "echo hi").unwrap();
    }

    #[test]
    fn read_claude_lines_argv() {
        let h = herdr();
        h.runner().push_ok(AGENT_READ);
        let out = h.read(&target(Kind::Claude, Some("bo"), "w8:p12"), 50).unwrap();
        assert_eq!(out, AGENT_READ);
        assert_eq!(
            h.runner().calls(),
            vec![sv(&[
                "herdr", "agent", "read", "bo", "--source", "recent-unwrapped", "--lines", "50"
            ])]
        );
    }

    #[test]
    fn read_shell_uses_pane_read() {
        let h = herdr();
        h.runner().push_ok("x");
        h.read(&target(Kind::Shell, None, "w8:p12"), 5).unwrap();
        assert_eq!(
            h.runner().calls(),
            vec![sv(&[
                "herdr", "pane", "read", "w8:p12", "--source", "recent-unwrapped", "--lines", "5"
            ])]
        );
    }

    #[test]
    fn wait_idle_argv() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.wait(
            &target(Kind::Claude, Some("bo"), "w8:p12"),
            AgentState::Idle,
            Duration::from_secs(600),
        )
        .unwrap();
        assert_eq!(
            h.runner().calls(),
            vec![sv(&[
                "herdr", "agent", "wait", "bo", "--until", "idle", "--until", "done", "--timeout",
                "600000"
            ])]
        );
    }

    #[test]
    fn wait_blocked_argv() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.wait(
            &target(Kind::Claude, Some("bo"), "w8:p12"),
            AgentState::Blocked,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(
            h.runner().calls(),
            vec![sv(&[
                "herdr", "agent", "wait", "bo", "--until", "blocked", "--timeout", "2000"
            ])]
        );
    }

    #[test]
    fn close_argv() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.close("w8:p12").unwrap();
        assert_eq!(h.runner().calls(), vec![sv(&["herdr", "pane", "close", "w8:p12"])]);
    }

    #[test]
    fn run_program_argv() {
        let h = herdr();
        h.runner().push_ok("{}");
        h.run_program("w8:p12", "htop").unwrap();
        assert_eq!(h.runner().calls(), vec![sv(&["herdr", "pane", "run", "w8:p12", "htop"])]);
    }

    #[test]
    fn current_pane_comes_from_env_or_errors() {
        assert_eq!(herdr().current_pane().unwrap(), "w8:pZ");
        let none = Herdr::new(FakeRunner::new(), None);
        assert!(none.current_pane().unwrap_err().to_string().contains("HERDR_PANE_ID"));
    }

    #[test]
    fn json_error_envelope_becomes_err() {
        let h = herdr();
        h.runner().push_ok(r#"{"error":{"code":"pane_not_found","message":"pane x not found"},"id":"i"}"#);
        let msg = h.close("x").unwrap_err().to_string();
        assert!(msg.contains("pane_not_found"), "{msg}");
    }

    #[test]
    fn herdr_send_passes_tricky_text_verbatim() {
        let text = "-x \"q\" $HOME `id`\nline2";
        for kind in [Kind::Claude, Kind::Shell] {
            let h = herdr();
            h.runner().push_ok("{}");
            h.send(&target(kind, Some("bo"), "w8:p12"), text).unwrap();
            let call = &h.runner().calls()[0];
            assert_eq!(call.last().unwrap(), text);
            assert_eq!(call.iter().filter(|a| a.as_str() == text).count(), 1);
            // herdr takes `--` as text, so it must not be passed.
            assert!(!call.iter().any(|a| a == "--"));
        }
    }
}
