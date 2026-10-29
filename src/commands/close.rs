//! `colony close`.

use super::log;
use crate::backend::is_pane_gone;
use crate::ctx::Ctx;
use crate::state::log::LogEvent;
use crate::state::names::{NAMES_FILE, Names};
use crate::target::resolve;
use anyhow::Result;

pub fn close(ctx: &Ctx, target: &str) -> Result<String> {
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let t = resolve(target, &names)?;
    // A pane that is already gone (or a backend that is not running) still drops the name.
    let gone = match ctx.backends.get(t.backend).close(&t.pane_id) {
        Ok(()) => false,
        Err(e) if is_pane_gone(&e) => true,
        Err(e) => return Err(e),
    };
    // Every name pointing at the pane goes (old behaviour).
    ctx.state.update_json(NAMES_FILE, 0o644, |n: &mut Names| {
        n.remove_pane(t.backend, &t.pane_id);
        Ok(())
    })?;
    log(
        ctx,
        &LogEvent::Close {
            target: target.to_string(),
            pane: t.pane_id.clone(),
        },
    );
    if gone {
        return Ok(format!(
            "Pane already gone: {target} ({}); name removed",
            t.pane_id
        ));
    }
    Ok(format!("Closed: {target} ({})", t.pane_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendKind, Kind};
    use crate::commands::testutil::Harness;

    #[test]
    fn close_by_name_kills_and_unregisters() {
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p2", None);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        let out = close(&h.ctx(), "bo").unwrap();
        assert_eq!(out, "Closed: bo (w1:p2)");
        assert!(h.backends.herdr.log.borrow().contains(&"close w1:p2".to_string()));
        assert!(h.names().get("bo").is_none());
    }

    #[test]
    fn close_by_raw_id_removes_all_names_on_pane() {
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p2", None);
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        // a second name on the same pane, written directly (insert would drop "bo")
        let mut extra = h.names().get("bo").unwrap().clone();
        extra.kind = Kind::Shell;
        h.state
            .update_json(crate::state::names::NAMES_FILE, 0o644, |n: &mut Names| {
                n.0.insert("alias".to_string(), extra);
                Ok(())
            })
            .unwrap();
        assert_eq!(h.names().0.len(), 2);
        let out = close(&h.ctx(), "w1:p2").unwrap();
        assert_eq!(out, "Closed: w1:p2 (w1:p2)");
        assert!(h.names().0.is_empty());
        assert!(h.backends.herdr.log.borrow().contains(&"close w1:p2".to_string()));
    }

    #[test]
    fn close_missing_pane_still_drops_name() {
        for msg in ["can't find pane: %0", "no server running on /tmp/tmux-1000/default"] {
            let h = Harness::new(&[("TMUX", "/tmp/x,1,0")]);
            h.add_name("ct-lead", BackendKind::Tmux, "%0", Kind::Other);
            *h.backends.tmux.close_error.borrow_mut() = Some(msg.to_string());
            let out = close(&h.ctx(), "ct-lead").unwrap();
            assert!(out.contains("already gone"), "{out}");
            assert!(h.names().get("ct-lead").is_none());
        }
    }

    #[test]
    fn close_other_error_fails_and_keeps_name() {
        let h = Harness::new(&[("TMUX", "/tmp/x,1,0")]);
        h.add_name("ct-lead", BackendKind::Tmux, "%0", Kind::Other);
        *h.backends.tmux.close_error.borrow_mut() = Some("permission denied".to_string());
        assert!(close(&h.ctx(), "ct-lead").is_err());
        assert!(h.names().get("ct-lead").is_some());
    }

    #[test]
    fn close_logged() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p1", Kind::Claude);
        close(&h.ctx(), "bo").unwrap();
        let log = h.log_text();
        assert!(log.contains("CLOSE target=bo pane=w1:p1"), "{log}");
    }
}
