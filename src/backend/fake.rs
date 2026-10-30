//! In-memory backend for tests. Every call is recorded in `log` as a string
//! such as `send <pane> <text>`.

use super::{AgentState, Backend, BackendError, BackendKind, Direction, PaneInfo, PaneRef};
use anyhow::{Result, anyhow};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

pub struct FakeBackend {
    kind: BackendKind,
    pub panes: RefCell<Vec<PaneInfo>>,
    pub log: RefCell<Vec<String>>,
    pub fail_list: Cell<bool>,
    pub fail_send: Cell<bool>,
    /// Makes `wait` fail as if the timeout expired.
    pub fail_wait: Cell<bool>,
    pub current: RefCell<String>,
    /// Text returned by `read`.
    pub read_text: RefCell<String>,
    /// Foreground program per pane, returned by `pane_command`.
    pub commands: RefCell<HashMap<String, String>>,
    /// Panes whose screen `screen_is_claude` reports as Claude.
    pub claude_screens: RefCell<Vec<String>>,
    /// Claude PIDs per pane, returned by `claude_pids`.
    pub pids: RefCell<HashMap<String, Vec<u32>>>,
    /// Makes `close` fail with this message.
    pub close_error: RefCell<Option<String>>,
    next_id: Cell<u32>,
}

impl FakeBackend {
    pub fn new(kind: BackendKind) -> Self {
        let current = match kind {
            BackendKind::Herdr => "w1:p1",
            BackendKind::Tmux => "%1",
        };
        Self {
            kind,
            panes: RefCell::new(vec![PaneInfo {
                pane_id: current.to_string(),
                ..Default::default()
            }]),
            log: RefCell::new(Vec::new()),
            fail_list: Cell::new(false),
            fail_send: Cell::new(false),
            fail_wait: Cell::new(false),
            current: RefCell::new(current.to_string()),
            read_text: RefCell::new(String::new()),
            commands: RefCell::new(HashMap::new()),
            claude_screens: RefCell::new(Vec::new()),
            pids: RefCell::new(HashMap::new()),
            close_error: RefCell::new(None),
            next_id: Cell::new(100),
        }
    }

    fn record(&self, entry: String) {
        self.log.borrow_mut().push(entry);
    }

    /// Log entries that start with `send `.
    pub fn sends(&self) -> Vec<String> {
        self.log
            .borrow()
            .iter()
            .filter(|l| l.starts_with("send "))
            .cloned()
            .collect()
    }

    pub fn add_pane(&self, pane_id: &str, agent: Option<(&str, AgentState)>) {
        self.panes.borrow_mut().push(PaneInfo {
            pane_id: pane_id.to_string(),
            agent: agent.map(|(k, s)| (k.to_string(), s)),
            ..Default::default()
        });
    }

    /// A herdr-style Claude pane (agent `claude`) with a terminal title.
    pub fn add_claude(&self, pane_id: &str, state: AgentState, title: Option<&str>) {
        self.panes.borrow_mut().push(PaneInfo {
            pane_id: pane_id.to_string(),
            agent: Some(("claude".to_string(), state)),
            title: title.map(str::to_string),
            ..Default::default()
        });
    }

    /// A tmux-style pane with a foreground command and a title.
    pub fn add_command_pane(&self, pane_id: &str, command: &str, title: Option<&str>) {
        self.panes.borrow_mut().push(PaneInfo {
            pane_id: pane_id.to_string(),
            command: Some(command.to_string()),
            title: title.map(str::to_string),
            ..Default::default()
        });
    }
}

impl Backend for FakeBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn current_pane(&self) -> Result<String> {
        Ok(self.current.borrow().clone())
    }

    fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        if self.fail_list.get() {
            return Err(anyhow!("fake list failure"));
        }
        Ok(self.panes.borrow().clone())
    }

    fn split(&self, from_pane: &str, dir: Direction, cwd: Option<&Path>) -> Result<String> {
        let n = self.next_id.get();
        self.next_id.set(n + 1);
        let id = match self.kind {
            BackendKind::Herdr => format!("w1:p{n}"),
            BackendKind::Tmux => format!("%{n}"),
        };
        self.panes.borrow_mut().push(PaneInfo {
            pane_id: id.clone(),
            ..Default::default()
        });
        let cwd = cwd.map(|p| p.display().to_string()).unwrap_or_default();
        self.record(format!("split {from_pane} {dir:?} {cwd}"));
        Ok(id)
    }

    fn split_column(&self, lead: &str, column: &[String], cwd: Option<&Path>) -> Result<String> {
        let n = self.next_id.get();
        self.next_id.set(n + 1);
        let id = match self.kind {
            BackendKind::Herdr => format!("w1:p{n}"),
            BackendKind::Tmux => format!("%{n}"),
        };
        self.panes.borrow_mut().push(PaneInfo {
            pane_id: id.clone(),
            ..Default::default()
        });
        let cwd = cwd.map(|p| p.display().to_string()).unwrap_or_default();
        match column.last() {
            None => self.record(format!("split_column {lead} right 50% {cwd}")),
            Some(last) => self.record(format!(
                "split_column {lead} below {last} equal={} {cwd}",
                column.len() + 1
            )),
        }
        Ok(id)
    }

    fn run_program(&self, pane: &str, cmd: &str) -> Result<()> {
        self.record(format!("run {pane} {cmd}"));
        Ok(())
    }

    fn start_claude(&self, pane: &str, name: &str, bypass: bool) -> Result<()> {
        self.record(format!("start_claude {pane} {name} bypass={bypass}"));
        for p in self.panes.borrow_mut().iter_mut() {
            if p.pane_id == pane {
                p.agent = Some(("claude".to_string(), AgentState::Idle));
            }
        }
        Ok(())
    }

    fn name_agent(&self, pane: &str, name: &str) -> Result<()> {
        self.record(format!("name {pane} {name}"));
        Ok(())
    }

    fn send(&self, target: &PaneRef, text: &str) -> Result<()> {
        if self.fail_send.get() {
            return Err(anyhow!("fake send failure"));
        }
        self.record(format!("send {} {}", target.pane_id, text));
        Ok(())
    }

    fn read(&self, target: &PaneRef, lines: usize) -> Result<String> {
        self.record(format!("read {} {}", target.pane_id, lines));
        Ok(self.read_text.borrow().clone())
    }

    fn wait(&self, target: &PaneRef, until: AgentState, timeout: Duration) -> Result<()> {
        if self.kind == BackendKind::Tmux {
            return Err(BackendError::HerdrOnly.into());
        }
        if self.fail_wait.get() {
            return Err(anyhow!("timed out waiting for {}", target.pane_id));
        }
        self.record(format!(
            "wait {} {until:?} {}s",
            target.pane_id,
            timeout.as_secs()
        ));
        Ok(())
    }

    fn close(&self, pane: &str) -> Result<()> {
        if let Some(m) = self.close_error.borrow().clone() {
            return Err(anyhow!(m));
        }
        self.record(format!("close {pane}"));
        self.panes.borrow_mut().retain(|p| p.pane_id != pane);
        Ok(())
    }

    fn agent_status(&self, target: &PaneRef) -> Result<Option<AgentState>> {
        Ok(self
            .panes
            .borrow()
            .iter()
            .find(|p| p.pane_id == target.pane_id)
            .and_then(|p| p.agent.as_ref().map(|(_, s)| *s)))
    }

    fn pane_command(&self, pane: &str) -> Result<Option<String>> {
        Ok(self.commands.borrow().get(pane).cloned())
    }

    fn screen_is_claude(&self, pane: &str) -> bool {
        self.claude_screens.borrow().iter().any(|p| p == pane)
    }

    fn claude_pids(&self, pane: &PaneInfo) -> Vec<u32> {
        self.pids.borrow().get(&pane.pane_id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_backend_split_adds_pane() {
        let b = FakeBackend::new(BackendKind::Herdr);
        let before = b.list_panes().unwrap().len();
        let id = b.split("w1:p1", Direction::Right, None).unwrap();
        let panes = b.list_panes().unwrap();
        assert_eq!(panes.len(), before + 1);
        assert!(panes.iter().any(|p| p.pane_id == id));
    }

    #[test]
    fn fake_backend_fail_list() {
        let b = FakeBackend::new(BackendKind::Tmux);
        b.fail_list.set(true);
        assert!(b.list_panes().is_err());
    }
}
