//! `pending.json`: reply ID -> where the answer goes. Valid 24 h, reusable.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PENDING_FILE: &str = "pending.json";
pub const REPLY_TTL: Duration = Duration::hours(24);

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct PendingEntry {
    pub target: String,
    pub chat_id: i64,
    pub message_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
#[serde(transparent)]
pub struct Pending(pub BTreeMap<String, PendingEntry>);

impl PendingEntry {
    fn is_valid(&self, now: DateTime<Utc>) -> bool {
        now - self.created_at <= REPLY_TTL
    }
}

impl Pending {
    /// Entry if it exists and is at most 24 h old. Does not consume it.
    pub fn get_valid(&self, id: &str, now: DateTime<Utc>) -> Option<&PendingEntry> {
        self.0.get(id).filter(|e| e.is_valid(now))
    }

    /// Drops expired entries (D-11).
    pub fn prune(&mut self, now: DateTime<Utc>) {
        self.0.retain(|_, e| e.is_valid(now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, 0, 0, 0).unwrap()
    }

    fn entry(created_at: DateTime<Utc>) -> PendingEntry {
        PendingEntry {
            target: "leader".to_string(),
            chat_id: 7,
            message_id: 99,
            created_at,
        }
    }

    #[test]
    fn pending_entry_has_all_fields() {
        let v = serde_json::to_value(entry(t0())).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["chat_id", "created_at", "message_id", "target"]);
    }

    #[test]
    fn pending_valid_at_23h59_and_reusable() {
        let mut p = Pending::default();
        p.0.insert("k3f9qa".to_string(), entry(t0()));
        let now = t0() + Duration::hours(23) + Duration::minutes(59);
        assert!(p.get_valid("k3f9qa", now).is_some());
        assert!(p.get_valid("k3f9qa", now).is_some());
    }

    #[test]
    fn pending_invalid_after_24h01() {
        let mut p = Pending::default();
        p.0.insert("k3f9qa".to_string(), entry(t0()));
        let now = t0() + Duration::hours(24) + Duration::minutes(1);
        assert!(p.get_valid("k3f9qa", now).is_none());
        assert!(p.get_valid("missing", now).is_none());
    }

    #[test]
    fn prune_removes_only_expired() {
        let mut p = Pending::default();
        p.0.insert("old".to_string(), entry(t0()));
        p.0.insert("new".to_string(), entry(t0() + Duration::hours(20)));
        p.prune(t0() + Duration::hours(25));
        assert!(p.0.contains_key("new"));
        assert!(!p.0.contains_key("old"));
    }
}
