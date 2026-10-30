//! `colony list`: table of registered panes; drops names whose pane is gone.

use super::{backend_str, kind_str, state_str};
use crate::backend::{BackendKind, Kind};
use crate::ctx::Ctx;
use crate::discover::discover;
use crate::state::names::{NAMES_FILE, Names};
use anyhow::Result;

const DASH: &str = "\u{2013}";

pub struct ListRow {
    pub name: String,
    pub pane_id: String,
    pub backend: BackendKind,
    pub kind: Kind,
    pub status: String,
    pub cwd: String,
    /// False for a Claude session found by discovery (never in `names.json`).
    pub registered: bool,
}

impl ListRow {
    /// `claude*` marks an unregistered session.
    pub fn kind_label(&self) -> String {
        format!("{}{}", kind_str(self.kind), if self.registered { "" } else { "*" })
    }
}

pub const UNREGISTERED_NOTE: &str = "* not registered";

/// Rows for every registered pane, then every unregistered Claude session the backends
/// can see (one `list_panes` per backend; an unreachable backend adds nothing). Stale names (backend answered, pane absent)
/// are removed from `names.json`; if a backend query fails the entries stay and
/// show status `?` (D-18).
pub fn list_rows(ctx: &Ctx) -> Result<Vec<ListRow>> {
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let mut rows = Vec::new();
    let mut stale: Vec<(BackendKind, String)> = Vec::new();
    for backend in [BackendKind::Herdr, BackendKind::Tmux] {
        let panes = ctx.backends.get(backend).list_panes();
        for (name, e) in names.0.iter().filter(|(_, e)| e.backend == backend) {
            let status = match &panes {
                Err(_) => "?".to_string(),
                Ok(ps) => match ps.iter().find(|p| p.pane_id == e.pane_id) {
                    None => {
                        stale.push((backend, e.pane_id.clone()));
                        continue;
                    }
                    Some(p) => match &p.agent {
                        Some((_, s)) => state_str(*s).to_string(),
                        None => DASH.to_string(),
                    },
                },
            };
            rows.push(ListRow {
                name: name.clone(),
                pane_id: e.pane_id.clone(),
                backend,
                kind: e.kind,
                status,
                cwd: e.cwd.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| DASH.to_string()),
                registered: true,
            });
        }
        if let Ok(ps) = &panes {
            for d in discover(ctx, backend, ps, &names) {
                rows.push(ListRow {
                    name: d.name,
                    pane_id: d.pane_id,
                    backend,
                    kind: Kind::Claude,
                    status: d.state.map_or_else(|| DASH.to_string(), |s| state_str(s).to_string()),
                    cwd: d.cwd.filter(|c| !c.is_empty()).unwrap_or_else(|| DASH.to_string()),
                    registered: false,
                });
            }
        }
    }
    if !stale.is_empty() {
        ctx.state.update_json(NAMES_FILE, 0o644, |n: &mut Names| {
            for (b, p) in &stale {
                n.remove_pane(*b, p);
            }
            Ok(())
        })?;
    }
    Ok(rows)
}

pub fn list(ctx: &Ctx) -> Result<String> {
    let rows = list_rows(ctx)?;
    let mut table: Vec<[String; 6]> = vec![[
        "NAME".into(),
        "PANE".into(),
        "BACKEND".into(),
        "KIND".into(),
        "STATUS".into(),
        "CWD".into(),
    ]];
    let any_unregistered = rows.iter().any(|r| !r.registered);
    for r in rows {
        table.push([
            r.name.clone(),
            r.pane_id.clone(),
            backend_str(r.backend).to_string(),
            r.kind_label(),
            r.status.clone(),
            r.cwd.clone(),
        ]);
    }
    let mut widths = [0usize; 6];
    for row in &table {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in &table {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i == 5 {
                line.push_str(cell);
            } else {
                line.push_str(cell);
                for _ in cell.chars().count()..widths[i] + 2 {
                    line.push(' ');
                }
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if any_unregistered {
        out.push_str(UNREGISTERED_NOTE);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentState;
    use crate::commands::testutil::Harness;

    #[test]
    fn list_has_six_columns() {
        let h = Harness::herdr();
        h.add_name("a", BackendKind::Herdr, "w1:p1", Kind::Shell);
        let out = list(&h.ctx()).unwrap();
        let header: Vec<&str> = out.lines().next().unwrap().split_whitespace().collect();
        assert_eq!(header, ["NAME", "PANE", "BACKEND", "KIND", "STATUS", "CWD"]);
        let row: Vec<&str> = out.lines().nth(1).unwrap().split_whitespace().collect();
        assert_eq!(row.len(), 6, "{out}");
        assert_eq!(&row[..4], ["a", "w1:p1", "herdr", "shell"]);
    }

    #[test]
    fn herdr_claude_shows_agent_status() {
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p2", Some(("claude", AgentState::Working)));
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        let out = list(&h.ctx()).unwrap();
        let row = out.lines().nth(1).unwrap();
        assert!(row.contains("working"), "{out}");
    }

    #[test]
    fn tmux_shows_dash() {
        let h = Harness::herdr();
        h.add_name("t", BackendKind::Tmux, "%1", Kind::Shell);
        let rows = list_rows(&h.ctx()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "\u{2013}");
        assert!(list(&h.ctx()).unwrap().contains('\u{2013}'));
    }

    #[test]
    fn stale_names_dropped_and_saved() {
        let h = Harness::herdr();
        h.add_name("alive", BackendKind::Herdr, "w1:p1", Kind::Shell);
        h.add_name("gone", BackendKind::Herdr, "w1:p9", Kind::Claude);
        let out = list(&h.ctx()).unwrap();
        assert!(out.contains("alive"), "{out}");
        assert!(!out.contains("gone"), "{out}");
        let names = h.names();
        assert!(names.get("gone").is_none());
        assert!(names.get("alive").is_some());
    }

    #[test]
    fn list_keeps_entries_when_backend_errors() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p9", Kind::Claude);
        h.backends.herdr.fail_list.set(true);
        let rows = list_rows(&h.ctx()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "?");
        assert!(h.names().get("bo").is_some());
    }

    #[test]
    fn no_main_tag() {
        let h = Harness::herdr();
        h.add_name("leader", BackendKind::Herdr, "w1:p1", Kind::Claude);
        let out = list(&h.ctx()).unwrap();
        assert!(!out.to_lowercase().contains("main"), "{out}");
    }
}
