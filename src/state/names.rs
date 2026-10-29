//! `names.json`: colony name -> pane.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NAMES_FILE: &str = "names.json";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Herdr,
    Tmux,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Shell,
    Other,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub backend: BackendKind,
    pub pane_id: String,
    pub kind: Kind,
    pub cwd: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Pane id of the pane that spawned this one (spawn layout column); absent in old files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<String>,
}

/// Names are stored and shown lowercase (D-2); lookups are case-insensitive.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
#[serde(transparent)]
pub struct Names(pub BTreeMap<String, Entry>);

impl Names {
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.0.get(&name.to_lowercase())
    }

    /// Inserts (overwriting the same name) and removes every other name that
    /// points at the same backend + pane id (D-3).
    pub fn insert(&mut self, name: &str, entry: Entry) {
        let key = name.to_lowercase();
        self.0
            .retain(|k, e| *k == key || !(e.backend == entry.backend && e.pane_id == entry.pane_id));
        self.0.insert(key, entry);
    }

    pub fn remove(&mut self, name: &str) -> Option<Entry> {
        self.0.remove(&name.to_lowercase())
    }

    /// Removes every name on that pane and returns the removed names.
    pub fn remove_pane(&mut self, backend: BackendKind, pane_id: &str) -> Vec<String> {
        let gone: Vec<String> = self
            .0
            .iter()
            .filter(|(_, e)| e.backend == backend && e.pane_id == pane_id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &gone {
            self.0.remove(k);
        }
        gone
    }

    /// Pane ids spawned by `spawner` on `backend`, in spawn order (created_at, then pane id).
    pub fn spawned_by(&self, backend: BackendKind, spawner: &str) -> Vec<String> {
        let mut v: Vec<&Entry> = self
            .0
            .values()
            .filter(|e| e.backend == backend && e.spawned_by.as_deref() == Some(spawner))
            .collect();
        v.sort_by_key(|e| (e.created_at, pane_seq(&e.pane_id), e.pane_id.clone()));
        v.into_iter().map(|e| e.pane_id.clone()).collect()
    }

    pub fn name_of_pane(&self, backend: BackendKind, pane_id: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, e)| e.backend == backend && e.pane_id == pane_id)
            .map(|(k, _)| k.as_str())
    }
}

/// Trailing number of a pane id (`w1:p100` -> 100, `%7` -> 7): ids grow with creation.
fn pane_seq(id: &str) -> u64 {
    let digits: String = id.chars().rev().take_while(char::is_ascii_digit).collect();
    digits.chars().rev().collect::<String>().parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::store::StateDir;
    use chrono::TimeZone;

    fn entry(backend: BackendKind, pane: &str) -> Entry {
        Entry {
            backend,
            pane_id: pane.to_string(),
            kind: Kind::Claude,
            cwd: None,
            created_at: Utc.with_ymd_and_hms(2026, 10, 29, 14, 52, 3).unwrap(),
            spawned_by: None,
        }
    }

    #[test]
    fn entry_json_has_five_fields() {
        let v = serde_json::to_value(entry(BackendKind::Herdr, "w1:p1")).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        let mut want = vec!["backend", "pane_id", "kind", "cwd", "created_at"];
        want.sort();
        assert_eq!(keys, want);
    }

    #[test]
    fn herdr_and_tmux_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateDir::new(dir.path().to_path_buf());
        s.update_json(NAMES_FILE, 0o644, |n: &mut Names| {
            n.insert("a", entry(BackendKind::Herdr, "w1:p1"));
            n.insert("b", entry(BackendKind::Tmux, "%3"));
            Ok(())
        })
        .unwrap();
        let n: Names = s.read_json(NAMES_FILE).unwrap();
        assert_eq!(n.get("a").unwrap().backend, BackendKind::Herdr);
        assert_eq!(n.get("b").unwrap().backend, BackendKind::Tmux);
        assert_eq!(n.0.len(), 2);
    }

    #[test]
    fn insert_same_pane_drops_old_name() {
        let mut n = Names::default();
        n.insert("old", entry(BackendKind::Herdr, "w1:p1"));
        n.insert("new", entry(BackendKind::Herdr, "w1:p1"));
        assert!(n.get("old").is_none());
        assert!(n.get("new").is_some());
        // same pane id on another backend is a different pane
        n.insert("other", entry(BackendKind::Tmux, "w1:p1"));
        assert!(n.get("new").is_some());
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut n = Names::default();
        n.insert("Bo-Server", entry(BackendKind::Tmux, "%1"));
        assert!(n.get("bo-server").is_some());
        assert!(n.get("BO-SERVER").is_some());
        assert!(n.0.contains_key("bo-server"));
        assert!(n.remove("BO-server").is_some());
    }

    #[test]
    fn name_of_pane_finds_sender() {
        let mut n = Names::default();
        n.insert("leader", entry(BackendKind::Herdr, "w1:p1"));
        assert_eq!(n.name_of_pane(BackendKind::Herdr, "w1:p1"), Some("leader"));
        assert_eq!(n.name_of_pane(BackendKind::Tmux, "w1:p1"), None);
        assert_eq!(n.remove_pane(BackendKind::Herdr, "w1:p1"), vec!["leader".to_string()]);
    }

    #[test]
    fn old_file_without_spawned_by_loads() {
        let old = r#"{"bo":{"backend":"herdr","pane_id":"w1:p2","kind":"claude","cwd":null,"created_at":"2026-10-29T14:52:03Z"}}"#;
        let n: Names = serde_json::from_str(old).unwrap();
        assert_eq!(n.get("bo").unwrap().spawned_by, None);
        assert!(!serde_json::to_string(&n).unwrap().contains("spawned_by"));
    }

    #[test]
    fn spawned_by_is_in_spawn_order() {
        let mut n = Names::default();
        for (name, pane) in [("a", "w1:p100"), ("b", "w1:p9"), ("c", "w1:p101")] {
            let mut e = entry(BackendKind::Herdr, pane);
            e.spawned_by = Some("w1:p1".into());
            n.insert(name, e);
        }
        n.insert("x", entry(BackendKind::Herdr, "w1:p50"));
        assert_eq!(n.spawned_by(BackendKind::Herdr, "w1:p1"), vec!["w1:p9", "w1:p100", "w1:p101"]);
        assert!(n.spawned_by(BackendKind::Tmux, "w1:p1").is_empty());
    }
}
