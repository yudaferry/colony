//! `colony.log` appender. One line per event:
//! `[2026-10-29 21:52:03 WIB] SEND key=value ...` (D-15).

use crate::ctx::Env;
use crate::state::store::StateDir;
use crate::time::wib_secs;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const PREVIEW_CHARS: usize = 120;

pub enum LogEvent {
    Send {
        from: String,
        to: String,
        to_pane: String,
        preview: String,
    },
    Reply {
        id: String,
        chat_id: i64,
    },
    Spawn {
        name: Option<String>,
        pane: String,
        cmd: String,
    },
    Close {
        target: String,
        pane: String,
    },
    Register {
        name: String,
        pane: String,
    },
}

/// Single-line, at most 120 chars.
fn preview(text: &str) -> String {
    text.chars()
        .take(PREVIEW_CHARS)
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Keeps a field on one line.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn format_line(now: DateTime<Utc>, ev: &LogEvent) -> String {
    let ts = wib_secs(now);
    match ev {
        LogEvent::Send {
            from,
            to,
            to_pane,
            preview: p,
        } => format!(
            "[{ts}] SEND from={} to={} pane={} preview=\"{}\"",
            one_line(from),
            one_line(to),
            one_line(to_pane),
            preview(p)
        ),
        LogEvent::Reply { id, chat_id } => {
            format!("[{ts}] REPLY id={} chat_id={chat_id}", one_line(id))
        }
        LogEvent::Spawn { name, pane, cmd } => format!(
            "[{ts}] SPAWN name={} pane={} cmd={}",
            one_line(name.as_deref().unwrap_or("-")),
            one_line(pane),
            one_line(if cmd.is_empty() { "shell" } else { cmd })
        ),
        LogEvent::Close { target, pane } => {
            format!("[{ts}] CLOSE target={} pane={}", one_line(target), one_line(pane))
        }
        LogEvent::Register { name, pane } => {
            format!("[{ts}] REGISTER name={} pane={}", one_line(name), one_line(pane))
        }
    }
}

pub fn log_event(path: &Path, now: DateTime<Utc>, ev: &LogEvent) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut line = format_line(now, ev);
    line.push('\n');
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// `COLONY_LOG_FILE` if set and non-empty, else `<state>/colony.log`.
pub fn log_path(state: &StateDir, env: &dyn Env) -> PathBuf {
    match env.var("COLONY_LOG_FILE") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => state.path("colony.log"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::MapEnv;
    use chrono::TimeZone;
    use std::collections::HashMap;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, 14, 52, 3).unwrap()
    }

    fn send(msg: &str) -> LogEvent {
        LogEvent::Send {
            from: "leader".to_string(),
            to: "bo".to_string(),
            to_pane: "w1:p2".to_string(),
            preview: msg.to_string(),
        }
    }

    #[test]
    fn each_event_appends_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("colony.log");
        let events = [
            send("hi"),
            LogEvent::Reply {
                id: "k3f9qa".to_string(),
                chat_id: 7,
            },
            LogEvent::Spawn {
                name: None,
                pane: "w1:p3".to_string(),
                cmd: "claude".to_string(),
            },
            LogEvent::Close {
                target: "bo".to_string(),
                pane: "w1:p2".to_string(),
            },
        ];
        for ev in &events {
            log_event(&path, now(), ev).unwrap();
        }
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 4);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn line_has_wib_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("colony.log");
        log_event(&path, now(), &send("hi")).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("[2026-10-29 21:52:03 WIB] SEND"),
            "got: {text}"
        );
    }

    #[test]
    fn preview_is_single_line_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("colony.log");
        let msg = "line\nof text ".repeat(50);
        assert!(msg.chars().count() > 500);
        log_event(&path, now(), &send(&msg)).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches('\n').count(), 1);
        let p = text
            .split("preview=\"")
            .nth(1)
            .unwrap()
            .trim_end()
            .trim_end_matches('"');
        assert!(p.chars().count() <= 120);
    }

    #[test]
    fn log_path_honours_override() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path().to_path_buf());
        let none = MapEnv(HashMap::new());
        assert_eq!(log_path(&state, &none), dir.path().join("colony.log"));
        let set = MapEnv(HashMap::from([(
            "COLONY_LOG_FILE".to_string(),
            "/tmp/x.log".to_string(),
        )]));
        assert_eq!(log_path(&state, &set), PathBuf::from("/tmp/x.log"));
    }
}
