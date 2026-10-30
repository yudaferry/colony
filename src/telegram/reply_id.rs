//! Reply IDs (6 chars `[a-z0-9]`) and their `pending.json` entries.

use crate::state::pending::{PENDING_FILE, Pending, PendingEntry};
use crate::state::store::StateDir;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
/// `pending.json` holds chat ids: keep it private like `telegram.json`.
pub const PENDING_MODE: u32 = 0o600;

fn random_id() -> String {
    // RandomState::new() carries fresh random keys on each call.
    let mut h = RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    h.write_u128(nanos);
    h.write_u32(std::process::id());
    let mut v = h.finish();
    let mut id = String::with_capacity(6);
    for _ in 0..6 {
        id.push(ALPHABET[(v % 36) as usize] as char);
        v /= 36;
    }
    id
}

/// A fresh 6-char `[a-z0-9]` id (inbox file names).
pub fn short_id() -> String {
    random_id()
}

/// A 6-char ID that is not already in `existing`.
pub fn new_reply_id(existing: &Pending) -> String {
    loop {
        let id = random_id();
        if !existing.0.contains_key(&id) {
            return id;
        }
    }
}

/// Prunes expired entries, inserts a new one and returns its ID.
pub fn create_pending(
    state: &StateDir,
    now: DateTime<Utc>,
    target: &str,
    chat_id: i64,
    message_id: i64,
) -> Result<String> {
    state.update_json(PENDING_FILE, PENDING_MODE, |p: &mut Pending| {
        p.prune(now);
        let id = new_reply_id(p);
        p.0.insert(
            id.clone(),
            PendingEntry {
                target: target.to_string(),
                chat_id,
                message_id,
                created_at: now,
            },
        );
        Ok(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use std::collections::HashSet;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, 14, 52, 3).unwrap()
    }

    fn store() -> (tempfile::TempDir, StateDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = StateDir::new(dir.path().to_path_buf());
        (dir, s)
    }

    #[test]
    fn id_is_6_shell_safe_chars() {
        for _ in 0..50 {
            let id = new_reply_id(&Pending::default());
            assert_eq!(id.len(), 6);
            assert!(id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        }
    }

    #[test]
    fn ids_unique_over_1000() {
        let mut p = Pending::default();
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let id = new_reply_id(&p);
            assert!(seen.insert(id.clone()), "duplicate {id}");
            p.0.insert(
                id,
                PendingEntry {
                    target: "t".to_string(),
                    chat_id: 1,
                    message_id: 1,
                    created_at: now(),
                },
            );
        }
    }

    #[test]
    fn create_pending_stores_all_fields() {
        let (_d, s) = store();
        let id = create_pending(&s, now(), "bo-server", 42, 7).unwrap();
        let p: Pending = s.read_json(PENDING_FILE).unwrap();
        let e = p.0.get(&id).unwrap();
        assert_eq!(e.target, "bo-server");
        assert_eq!(e.chat_id, 42);
        assert_eq!(e.message_id, 7);
        assert_eq!(e.created_at, now());
    }

    #[test]
    fn create_pending_prunes_expired() {
        let (_d, s) = store();
        let old = create_pending(&s, now() - Duration::hours(30), "a", 1, 1).unwrap();
        let fresh = create_pending(&s, now(), "b", 2, 2).unwrap();
        let p: Pending = s.read_json(PENDING_FILE).unwrap();
        assert!(!p.0.contains_key(&old));
        assert!(p.0.contains_key(&fresh));
    }
}
