//! `colony read`.

use crate::ctx::Ctx;
use crate::state::names::{NAMES_FILE, Names};
use crate::target::resolve;
use anyhow::Result;

pub fn read(ctx: &Ctx, target: &str, lines: usize) -> Result<String> {
    let names: Names = ctx.state.read_json(NAMES_FILE)?;
    let t = resolve(target, &names)?;
    let text = ctx.backends.get(t.backend).read(&t, lines)?;
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    Ok(all[start..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendKind, Kind};
    use crate::commands::testutil::Harness;

    #[test]
    fn read_herdr_claude() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        *h.backends.herdr.read_text.borrow_mut() = "hello\nworld".to_string();
        assert_eq!(read(&h.ctx(), "bo", 50).unwrap(), "hello\nworld");
        assert!(h.backends.herdr.log.borrow().contains(&"read w1:p2 50".to_string()));
    }

    #[test]
    fn read_tmux() {
        let h = Harness::herdr();
        h.add_name("tw", BackendKind::Tmux, "%1", Kind::Shell);
        *h.backends.tmux.read_text.borrow_mut() = "$ ls".to_string();
        assert_eq!(read(&h.ctx(), "tw", 5).unwrap(), "$ ls");
        assert!(h.backends.herdr.log.borrow().is_empty());
    }

    #[test]
    fn read_lines_limit() {
        let h = Harness::herdr();
        h.add_name("bo", BackendKind::Herdr, "w1:p2", Kind::Claude);
        let text: Vec<String> = (1..=100).map(|i| format!("line{i}")).collect();
        *h.backends.herdr.read_text.borrow_mut() = text.join("\n");
        let out = read(&h.ctx(), "bo", 10).unwrap();
        assert_eq!(out.lines().count(), 10);
        assert!(out.starts_with("line91"), "{out}");
        assert!(out.ends_with("line100"), "{out}");
    }
}
