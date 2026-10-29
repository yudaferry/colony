//! Resolve a `<name|id>` argument to a `PaneRef` (D-1, D-2).

use crate::backend::{BackendKind, Kind, PaneRef};
use crate::state::names::Names;
use anyhow::{Result, anyhow};

/// `%` followed by digits.
fn is_tmux_id(s: &str) -> bool {
    s.strip_prefix('%')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// `w<ws>:p<id>`, both parts alphanumeric.
fn is_herdr_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('w') else {
        return false;
    };
    let Some((ws, pane)) = rest.split_once(":p") else {
        return false;
    };
    let alnum = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric());
    alnum(ws) && alnum(pane)
}

/// Name first (stored backend wins, FR-2); then a raw tmux or herdr pane id.
pub fn resolve(arg: &str, names: &Names) -> Result<PaneRef> {
    if let Some(e) = names.get(arg) {
        return Ok(PaneRef {
            backend: e.backend,
            pane_id: e.pane_id.clone(),
            name: Some(arg.to_lowercase()),
            kind: e.kind,
        });
    }
    let backend = if is_tmux_id(arg) {
        BackendKind::Tmux
    } else if is_herdr_id(arg) {
        BackendKind::Herdr
    } else {
        return Err(anyhow!(
            "no pane registered with name '{arg}'. Run: colony register {arg} <pane-id>"
        ));
    };
    // A raw id that some name points at takes that name's kind.
    if let Some(name) = names.name_of_pane(backend, arg)
        && let Some(e) = names.get(name)
    {
        return Ok(PaneRef {
            backend,
            pane_id: arg.to_string(),
            name: Some(name.to_string()),
            kind: e.kind,
        });
    }
    Ok(PaneRef {
        backend,
        pane_id: arg.to_string(),
        name: None,
        kind: Kind::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::names::Entry;
    use chrono::{TimeZone, Utc};

    fn entry(backend: BackendKind, pane: &str, kind: Kind) -> Entry {
        Entry {
            backend,
            pane_id: pane.to_string(),
            kind,
            cwd: None,
            created_at: Utc.with_ymd_and_hms(2026, 10, 29, 14, 52, 3).unwrap(),
            spawned_by: None,
        }
    }

    #[test]
    fn name_uses_stored_backend() {
        let mut n = Names::default();
        n.insert("Worker", entry(BackendKind::Tmux, "%4", Kind::Claude));
        let r = resolve("worker", &n).unwrap();
        assert_eq!(r.backend, BackendKind::Tmux);
        assert_eq!(r.pane_id, "%4");
        assert_eq!(r.name.as_deref(), Some("worker"));
        assert_eq!(r.kind, Kind::Claude);
    }

    #[test]
    fn raw_tmux_id() {
        let r = resolve("%12", &Names::default()).unwrap();
        assert_eq!(r.backend, BackendKind::Tmux);
        assert_eq!(r.pane_id, "%12");
        assert_eq!(r.name, None);
        assert_eq!(r.kind, Kind::Other);
    }

    #[test]
    fn raw_herdr_id() {
        let r = resolve("w8:p12", &Names::default()).unwrap();
        assert_eq!(r.backend, BackendKind::Herdr);
        assert_eq!(r.pane_id, "w8:p12");
        assert_eq!(r.kind, Kind::Other);
        let r = resolve("wB:pN", &Names::default()).unwrap();
        assert_eq!(r.backend, BackendKind::Herdr);
    }

    #[test]
    fn raw_id_known_to_names_takes_its_kind() {
        let mut n = Names::default();
        n.insert("bo", entry(BackendKind::Herdr, "w8:p2", Kind::Claude));
        let r = resolve("w8:p2", &n).unwrap();
        assert_eq!(r.kind, Kind::Claude);
        assert_eq!(r.name.as_deref(), Some("bo"));
    }

    #[test]
    fn unknown_name_errors_with_register_hint() {
        let msg = resolve("ghost", &Names::default()).unwrap_err().to_string();
        assert!(msg.contains("no pane registered with name 'ghost'"), "{msg}");
        assert!(msg.contains("colony register ghost <pane-id>"), "{msg}");
    }
}
