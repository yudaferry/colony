//! `sent.json` (mode 600): Telegram message id -> the session that sent it with
//! `colony telegram reply`, so a swipe-reply goes back to that session. Valid 7 days.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SENT_FILE: &str = "sent.json";
pub const SENT_MODE: u32 = 0o600;
pub const SENT_TTL: Duration = Duration::days(7);

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SentEntry {
    /// Resolvable target: registered name, discovered name or pane id.
    pub target: String,
    pub created_at: DateTime<Utc>,
}

/// Keyed by `"<chat_id>:<message_id>"`.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
#[serde(transparent)]
pub struct Sent(pub BTreeMap<String, SentEntry>);

fn key(chat_id: i64, message_id: i64) -> String {
    format!("{chat_id}:{message_id}")
}

impl Sent {
    fn valid(e: &SentEntry, now: DateTime<Utc>) -> bool {
        now - e.created_at <= SENT_TTL
    }

    /// Target of a message the bot sent for a session, if known and not expired.
    pub fn target_of(&self, chat_id: i64, message_id: i64, now: DateTime<Utc>) -> Option<&str> {
        self.0
            .get(&key(chat_id, message_id))
            .filter(|e| Self::valid(e, now))
            .map(|e| e.target.as_str())
    }

    /// Drops expired entries.
    pub fn prune(&mut self, now: DateTime<Utc>) {
        self.0.retain(|_, e| Self::valid(e, now));
    }

    /// Prunes, then records every id in `message_ids`.
    pub fn record(&mut self, now: DateTime<Utc>, chat_id: i64, message_ids: &[i64], target: &str) {
        self.prune(now);
        for id in message_ids {
            self.0.insert(
                key(chat_id, *id),
                SentEntry {
                    target: target.to_string(),
                    created_at: now,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::store::StateDir;
    use chrono::TimeZone;
    use std::os::unix::fs::PermissionsExt;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, 0, 0, 0).unwrap()
    }

    #[test]
    fn lookup_expires_after_7_days_and_prune_drops_old() {
        let mut s = Sent::default();
        s.record(t0(), 1, &[10, 11], "bo");
        assert_eq!(s.target_of(1, 10, t0() + Duration::days(6)), Some("bo"));
        assert_eq!(s.target_of(1, 10, t0() + Duration::days(8)), None);
        assert_eq!(s.target_of(2, 10, t0()), None);
        s.record(t0() + Duration::days(8), 1, &[12], "x");
        assert_eq!(s.0.len(), 1);
        assert!(s.0.contains_key("1:12"));
    }

    #[test]
    fn file_is_600_and_pruned_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let st = StateDir::new(dir.path().to_path_buf());
        st.update_json(SENT_FILE, SENT_MODE, |s: &mut Sent| {
            s.record(t0(), 1, &[1], "old");
            Ok(())
        })
        .unwrap();
        st.update_json(SENT_FILE, SENT_MODE, |s: &mut Sent| {
            s.record(t0() + Duration::days(9), 1, &[2], "new");
            Ok(())
        })
        .unwrap();
        let mode = std::fs::metadata(st.path(SENT_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let s: Sent = st.read_json(SENT_FILE).unwrap();
        assert_eq!(s.0.len(), 1);
        assert!(s.0.contains_key("1:2"));
    }
}
