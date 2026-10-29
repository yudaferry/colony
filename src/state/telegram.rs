//! `telegram.json` (mode 600): token, lock, receiver.

use serde::{Deserialize, Serialize};

pub const TELEGRAM_FILE: &str = "telegram.json";
pub const TELEGRAM_MODE: u32 = 0o600;

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
pub struct TelegramConfig {
    pub token: Option<String>,
    pub locked_user_id: Option<i64>,
    pub locked_chat_id: Option<i64>,
    pub receiver: Option<String>,
    /// Unix time of the last `colony telegram connect`. Only messages sent at or
    /// after it may claim the lock; older ones are ignored (D-15).
    #[serde(default)]
    pub connected_at: Option<i64>,
    /// Next `getUpdates` offset, persisted so a restart does not replay handled updates.
    #[serde(default)]
    pub offset: i64,
    /// The bot `offset` belongs to; a different bot id resets the offset.
    #[serde(default)]
    pub offset_bot_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::store::StateDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn telegram_file_is_600_after_each_write() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateDir::new(dir.path().to_path_buf());
        let check = || {
            let mode = std::fs::metadata(s.path(TELEGRAM_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        };
        s.update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            c.token = Some("123:abc".to_string());
            Ok(())
        })
        .unwrap();
        check();
        s.update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            c.locked_user_id = Some(42);
            c.locked_chat_id = Some(42);
            Ok(())
        })
        .unwrap();
        check();
        s.update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
            c.receiver = Some("leader".to_string());
            Ok(())
        })
        .unwrap();
        check();
        let c: TelegramConfig = s.read_json(TELEGRAM_FILE).unwrap();
        assert_eq!(c.token.as_deref(), Some("123:abc"));
        assert_eq!(c.locked_user_id, Some(42));
        assert_eq!(c.receiver.as_deref(), Some("leader"));
    }
}
