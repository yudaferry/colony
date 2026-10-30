//! WIB (UTC+7, Asia/Jakarta) formatting for everything shown to the user.

use chrono::{DateTime, Utc};
use chrono_tz::Asia::Jakarta;

/// `2026-10-29 21:52 WIB`
pub fn wib(dt: DateTime<Utc>) -> String {
    format!("{} WIB", dt.with_timezone(&Jakarta).format("%Y-%m-%d %H:%M"))
}

/// `2026-10-29 21:52:03 WIB`
pub fn wib_secs(dt: DateTime<Utc>) -> String {
    format!(
        "{} WIB",
        dt.with_timezone(&Jakarta).format("%Y-%m-%d %H:%M:%S")
    )
}

/// `2026-10-29_215203` (WIB), for file names.
pub fn file_stamp(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Jakarta).format("%Y-%m-%d_%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn wib_formats_utc_plus_7() {
        let dt = Utc.with_ymd_and_hms(2026, 10, 29, 14, 52, 3).unwrap();
        assert_eq!(wib(dt), "2026-10-29 21:52 WIB");
        assert_eq!(wib_secs(dt), "2026-10-29 21:52:03 WIB");
    }

    #[test]
    fn wib_crosses_midnight() {
        let dt = Utc.with_ymd_and_hms(2026, 10, 29, 18, 30, 0).unwrap();
        assert_eq!(wib(dt), "2026-10-30 01:30 WIB");
    }
}
