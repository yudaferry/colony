//! `schedule.json` (mode 600): the scheduled jobs, plus the pure cron/timezone logic.
//! Cron parsing and next/previous fire times come from the `croner` crate.

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use croner::parser::{CronParser, Seconds};
use serde::{Deserialize, Serialize};
use super::names::BackendKind;

pub const SCHEDULE_FILE: &str = "schedule.json";
pub const SCHEDULE_MODE: u32 = 0o600;
pub const DEFAULT_TZ: &str = "Asia/Jakarta";
/// Default timeout of a command job.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_ID_LEN: usize = 32;

fn default_tz() -> String {
    DEFAULT_TZ.to_string()
}

fn default_true() -> bool {
    true
}

/// What a job does when it fires.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum JobKind {
    /// Send `message` to session `target` as if it came from the phone.
    Message { target: String, message: String },
    /// Run `command` with `sh -c`; alert the phone on non-zero exit or any stdout.
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify_target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Job {
    pub id: String,
    /// 5-field cron expression.
    pub cron: String,
    #[serde(default = "default_tz")]
    pub tz: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub kind: JobKind,
    /// Unix time of the scheduled minute that fired last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<i64>,
    /// The job removes itself after this many runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<u32>,
    /// Runs so far (counted for `times`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub runs_done: u32,
    /// Unix time: no runs before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<i64>,
    /// Unix time: the job removes itself once it is passed; no run at or after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    /// Unix time: runs are skipped (not replayed) until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<i64>,
    /// Who added the job and where its notices go. Absent in old files (notices go to Telegram).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<CreatedBy>,
}

/// Where the notices of a job (finished, error) go.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum JobOrigin {
    /// The locked Telegram chat (or `CreatedBy::chat_id` when stored).
    Telegram,
    /// The creating session (`backend` + `pane_id`); Telegram when it no longer exists.
    Session,
    /// Nobody asked: behaves like Telegram when a chat is locked.
    None,
}

/// The creator of a job.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct CreatedBy {
    /// Session name (registered, else discovered, else pane id), or `user` / `telegram`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    pub origin: JobOrigin,
    /// Chat of the `--from telegram:<reply-id>` reply id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<i64>,
}

impl CreatedBy {
    /// `by <name> via <telegram|session>` for `schedule list`.
    pub fn label(&self) -> String {
        let via = match self.origin {
            JobOrigin::Telegram => "telegram",
            JobOrigin::Session => "session",
            JobOrigin::None => "none",
        };
        format!("by {} via {via}", self.name)
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Optional limits of a new job (unix times).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Limits {
    pub times: Option<u32>,
    pub from: Option<i64>,
    pub until: Option<i64>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
#[serde(transparent)]
pub struct Schedule(pub Vec<Job>);

impl Schedule {
    pub fn get(&self, id: &str) -> Option<&Job> {
        self.0.iter().find(|j| j.id == id)
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Job> {
        self.0.iter_mut().find(|j| j.id == id)
    }
}

impl Job {
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            JobKind::Message { .. } => "message",
            JobKind::Command { .. } => "command",
        }
    }

    pub fn timeout_secs(&self) -> u64 {
        match &self.kind {
            JobKind::Command { timeout_secs: Some(t), .. } if *t > 0 => *t,
            _ => DEFAULT_TIMEOUT_SECS,
        }
    }

    /// Previous fire time (UTC) at or before `now`, in this job's timezone.
    pub fn prev_fire(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        let (cron, tz) = (parse_cron(&self.cron)?, parse_tz(&self.tz)?);
        Ok(cron
            .find_previous_occurrence(&now.with_timezone(&tz), true)
            .ok()
            .map(|d| d.with_timezone(&Utc)))
    }

    /// Next fire time (UTC) strictly after `now`.
    pub fn next_fire(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        let (cron, tz) = (parse_cron(&self.cron)?, parse_tz(&self.tz)?);
        Ok(cron
            .find_next_occurrence(&now.with_timezone(&tz), false)
            .ok()
            .map(|d| d.with_timezone(&Utc)))
    }

    /// True while a pause is active at `now`.
    pub fn is_paused(&self, now: DateTime<Utc>) -> bool {
        self.paused_until.is_some_and(|u| now.timestamp() < u)
    }

    /// True once `until` is passed or all `times` runs are done: the job is to be removed.
    pub fn is_finished(&self, now: DateTime<Utc>) -> bool {
        self.until.is_some_and(|u| now.timestamp() >= u) || self.times.is_some_and(|t| self.runs_done >= t)
    }

    /// Runs left when `times` is set.
    pub fn runs_left(&self) -> Option<u32> {
        self.times.map(|t| t.saturating_sub(self.runs_done))
    }

    /// The latest fire time at or before `now` that is newer than `last_run` and not
    /// older than `since` (the bot start: jobs missed while it was stopped are not replayed).
    fn newest_slot(&self, now: DateTime<Utc>, since: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        Ok(self.prev_fire(now)?.filter(|p| {
            *p >= since && self.last_run.is_none_or(|l| p.timestamp() > l)
        }))
    }

    /// The scheduled minute to fire now, if any. Nothing while disabled, paused or finished,
    /// before `from`, or at/after `until`.
    pub fn due(&self, now: DateTime<Utc>, since: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        if !self.enabled || self.is_paused(now) || self.is_finished(now) {
            return Ok(None);
        }
        Ok(self.newest_slot(now, since)?.filter(|p| {
            self.from.is_none_or(|f| p.timestamp() >= f) && self.until.is_none_or(|u| p.timestamp() < u)
        }))
    }

    /// The scheduled minute that passes while the job is paused: it is skipped, and recorded
    /// as done so it is never replayed after the pause.
    pub fn skipped(&self, now: DateTime<Utc>, since: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        if !self.enabled || !self.is_paused(now) {
            return Ok(None);
        }
        self.newest_slot(now, since)
    }

    /// Next run (UTC) after `now`, honouring `from`, an active pause, `until` and `times`.
    pub fn next_run(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        if self.is_finished(now) {
            return Ok(None);
        }
        let mut base = now;
        for t in [self.from, self.paused_until].into_iter().flatten() {
            if let Some(b) = DateTime::from_timestamp(t - 1, 0) {
                base = base.max(b);
            }
        }
        Ok(self.next_fire(base)?.filter(|n| self.until.is_none_or(|u| n.timestamp() < u)))
    }
}

/// Parses exactly 5 cron fields (minute hour day-of-month month day-of-week).
pub fn parse_cron(expr: &str) -> Result<Cron> {
    let fields = expr.split_whitespace().count();
    if fields != 5 {
        bail!("cron must have exactly 5 fields (minute hour day month weekday), got {fields}: '{expr}'");
    }
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .build()
        .parse(expr)
        .map_err(|e| anyhow!("invalid cron '{expr}': {e}"))
}

pub fn parse_tz(tz: &str) -> Result<Tz> {
    tz.parse::<Tz>().map_err(|_| anyhow!("unknown timezone '{tz}' (use an IANA name such as Asia/Jakarta)"))
}

/// Ids are used in buttons and on the command line: `[A-Za-z0-9_-]`, at most 32.
pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > MAX_ID_LEN
        || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("job id must be 1-{MAX_ID_LEN} characters of letters, digits, '-' or '_'");
    }
    Ok(())
}

/// `YYYY-MM-DD HH:MM` in `tz` -> unix time.
pub fn parse_local(text: &str, tz: &str) -> Result<i64> {
    let zone = parse_tz(tz)?;
    let naive = NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M")
        .map_err(|_| anyhow!("invalid time '{text}' (use \"YYYY-MM-DD HH:MM\", e.g. \"2026-10-30 18:00\")"))?;
    zone.from_local_datetime(&naive)
        .earliest()
        .map(|d| d.timestamp())
        .ok_or_else(|| anyhow!("'{text}' does not exist in {tz} (daylight saving gap)"))
}

/// `30m`, `2h`, `1d` -> duration (at least 1 minute, at most 365 days).
pub fn parse_duration(text: &str) -> Result<Duration> {
    let t = text.trim();
    let bad = || anyhow!("invalid duration '{text}' (use a number and m, h or d: 30m, 2h, 1d)");
    let unit = t.chars().last().ok_or_else(bad)?;
    let n: i64 = t[..t.len() - unit.len_utf8()].parse().map_err(|_| bad())?;
    let minutes = match unit {
        'm' => n,
        'h' => n.checked_mul(60).ok_or_else(bad)?,
        'd' => n.checked_mul(1440).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    if !(1..=365 * 1440).contains(&minutes) {
        bail!("duration '{text}' must be between 1m and 365d");
    }
    Ok(Duration::minutes(minutes))
}

/// Validates and appends a job without limits.
pub fn add_job(
    state: &super::store::StateDir,
    id: &str,
    cron: &str,
    tz: &str,
    kind: JobKind,
) -> Result<()> {
    add_job_with(state, id, cron, tz, kind, Limits::default(), None)
}

/// Validates and appends a job. Errors when the id exists or cron/tz/id/fields/limits are invalid.
pub fn add_job_with(
    state: &super::store::StateDir,
    id: &str,
    cron: &str,
    tz: &str,
    kind: JobKind,
    limits: Limits,
    created_by: Option<CreatedBy>,
) -> Result<()> {
    validate_id(id)?;
    parse_cron(cron)?;
    parse_tz(tz)?;
    if limits.times == Some(0) {
        bail!("--times must be at least 1");
    }
    if let (Some(f), Some(u)) = (limits.from, limits.until) {
        if u <= f {
            bail!("--until must be after --from");
        }
    }
    match &kind {
        JobKind::Message { target, message } if target.trim().is_empty() || message.trim().is_empty() => {
            bail!("target and message must not be empty")
        }
        JobKind::Command { command, .. } if command.trim().is_empty() => bail!("command must not be empty"),
        _ => {}
    }
    state.update_json(SCHEDULE_FILE, SCHEDULE_MODE, |s: &mut Schedule| {
        if s.get(id).is_some() {
            bail!("job '{id}' already exists");
        }
        s.0.push(Job {
            id: id.to_string(),
            cron: cron.split_whitespace().collect::<Vec<_>>().join(" "),
            tz: tz.to_string(),
            enabled: true,
            kind,
            last_run: None,
            times: limits.times,
            runs_done: 0,
            from: limits.from,
            until: limits.until,
            paused_until: None,
            created_by,
        });
        Ok(())
    })
}

/// Pauses a job until the unix time `until` (runs are skipped, not replayed).
pub fn pause_job(state: &super::store::StateDir, id: &str, until: i64) -> Result<()> {
    state.update_json(SCHEDULE_FILE, SCHEDULE_MODE, |s: &mut Schedule| match s.get_mut(id) {
        Some(j) => {
            j.paused_until = Some(until);
            Ok(())
        }
        None => bail!("no job '{id}'"),
    })
}

/// Ends a pause early. Returns false when the job was not paused. The scheduled minute
/// that passed during the pause is marked done so it is not replayed.
pub fn resume_job(state: &super::store::StateDir, id: &str, now: DateTime<Utc>) -> Result<bool> {
    state.update_json(SCHEDULE_FILE, SCHEDULE_MODE, |s: &mut Schedule| match s.get_mut(id) {
        Some(j) => {
            if !j.is_paused(now) {
                j.paused_until = None;
                return Ok(false);
            }
            j.paused_until = None;
            if let Ok(Some(p)) = j.prev_fire(now) {
                j.last_run = Some(j.last_run.map_or(p.timestamp(), |l| l.max(p.timestamp())));
            }
            Ok(true)
        }
        None => bail!("no job '{id}'"),
    })
}

pub fn remove_job(state: &super::store::StateDir, id: &str) -> Result<()> {
    state.update_json(SCHEDULE_FILE, SCHEDULE_MODE, |s: &mut Schedule| {
        let before = s.0.len();
        s.0.retain(|j| j.id != id);
        if s.0.len() == before {
            bail!("no job '{id}'");
        }
        Ok(())
    })
}

pub fn set_enabled(state: &super::store::StateDir, id: &str, on: bool) -> Result<()> {
    state.update_json(SCHEDULE_FILE, SCHEDULE_MODE, |s: &mut Schedule| {
        match s.get_mut(id) {
            Some(j) => {
                j.enabled = on;
                Ok(())
            }
            None => bail!("no job '{id}'"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn job(cron: &str, tz: &str) -> Job {
        Job {
            id: "j".into(),
            cron: cron.into(),
            tz: tz.into(),
            enabled: true,
            kind: JobKind::Message { target: "bo".into(), message: "hi".into() },
            last_run: None,
            times: None,
            runs_done: 0,
            from: None,
            until: None,
            paused_until: None,
            created_by: None,
        }
    }

    fn utc(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, h, m, s).unwrap()
    }

    #[test]
    fn cron_validation() {
        assert!(parse_cron("0 9 * * 1-5").is_ok());
        assert!(parse_cron("*/5 * * * *").is_ok());
        assert!(parse_cron("0 0 9 * * *").is_err(), "6 fields refused");
        assert!(parse_cron("* * * *").is_err());
        assert!(parse_cron("61 * * * *").is_err());
        assert!(parse_cron("nonsense").is_err());
        assert!(parse_tz("Asia/Jakarta").is_ok());
        assert!(parse_tz("Mars/Base").is_err());
    }

    #[test]
    fn next_fire_respects_timezone() {
        // 09:00 Jakarta = 02:00 UTC
        let j = job("0 9 * * *", "Asia/Jakarta");
        assert_eq!(j.next_fire(utc(1, 0, 0)).unwrap(), Some(utc(2, 0, 0)));
        // 09:00 New York (EDT, UTC-4 on 2026-10-29) = 13:00 UTC
        let j = job("0 9 * * *", "America/New_York");
        assert_eq!(j.next_fire(utc(1, 0, 0)).unwrap(), Some(utc(13, 0, 0)));
    }

    #[test]
    fn due_once_per_scheduled_minute() {
        let mut j = job("*/10 * * * *", "Asia/Jakarta");
        let since = utc(0, 0, 0);
        let now = utc(3, 20, 30);
        let p = j.due(now, since).unwrap().unwrap();
        assert_eq!(p, utc(3, 20, 0));
        j.last_run = Some(p.timestamp());
        // Same minute, later ticks: nothing.
        assert_eq!(j.due(utc(3, 20, 59), since).unwrap(), None);
        assert_eq!(j.due(utc(3, 25, 0), since).unwrap(), None);
        // Next slot fires again.
        assert_eq!(j.due(utc(3, 30, 1), since).unwrap(), Some(utc(3, 30, 0)));
    }

    #[test]
    fn missed_while_stopped_not_replayed_and_disabled_skipped() {
        let mut j = job("0 2 * * *", "UTC");
        // Bot started 02:30; the 02:00 slot is before it.
        assert_eq!(j.due(utc(2, 31, 0), utc(2, 30, 0)).unwrap(), None);
        assert_eq!(j.due(utc(2, 0, 10), utc(1, 59, 0)).unwrap(), Some(utc(2, 0, 0)));
        j.enabled = false;
        assert_eq!(j.due(utc(2, 0, 10), utc(1, 59, 0)).unwrap(), None);
    }

    #[test]
    fn job_json_shape_and_defaults() {
        let j: Job = serde_json::from_str(r#"{"id":"a","cron":"* * * * *","kind":"command","command":"true"}"#).unwrap();
        assert_eq!(j.tz, "Asia/Jakarta");
        assert!(j.enabled);
        assert_eq!(j.timeout_secs(), 120);
        let v = serde_json::to_value(job("* * * * *", "UTC")).unwrap();
        assert_eq!(v["kind"], "message");
        assert_eq!(v["target"], "bo");
    }

    #[test]
    fn parse_times_and_durations() {
        // 2026-10-30 18:00 WIB = 11:00 UTC
        assert_eq!(parse_local("2026-10-30 18:00", "Asia/Jakarta").unwrap(), utc(0, 0, 0).timestamp() + 86400 + 11 * 3600);
        assert_eq!(parse_local("2026-10-30 18:00", "UTC").unwrap(), utc(0, 0, 0).timestamp() + 86400 + 18 * 3600);
        for bad in ["2026-10-30", "18:00", "2026-13-01 10:00", "tomorrow", ""] {
            assert!(parse_local(bad, "UTC").is_err(), "{bad}");
        }
        assert!(parse_local("2026-10-30 18:00", "Mars/Base").is_err());
        assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
        assert_eq!(parse_duration("1d").unwrap(), Duration::days(1));
        for bad in ["", "m", "0m", "-5m", "2x", "1.5h", "30", "999d", "h2"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn from_blocks_early_runs_until_blocks_late_runs() {
        let since = utc(0, 0, 0);
        let mut j = job("*/10 * * * *", "UTC");
        j.from = Some(utc(3, 30, 0).timestamp());
        assert_eq!(j.due(utc(3, 20, 5), since).unwrap(), None, "before from");
        assert_eq!(j.due(utc(3, 30, 5), since).unwrap(), Some(utc(3, 30, 0)), "at from");
        assert_eq!(j.next_run(utc(3, 0, 0)).unwrap(), Some(utc(3, 30, 0)));
        let mut j = job("*/10 * * * *", "UTC");
        j.until = Some(utc(4, 0, 0).timestamp());
        assert_eq!(j.due(utc(3, 50, 5), since).unwrap(), Some(utc(3, 50, 0)));
        assert_eq!(j.due(utc(4, 0, 5), since).unwrap(), None, "no run at until");
        assert!(j.is_finished(utc(4, 0, 0)) && !j.is_finished(utc(3, 59, 59)));
        assert_eq!(j.next_run(utc(3, 55, 0)).unwrap(), None, "next slot is at until");
        assert_eq!(j.next_run(utc(3, 45, 0)).unwrap(), Some(utc(3, 50, 0)));
    }

    #[test]
    fn times_finish_the_job() {
        let mut j = job("* * * * *", "UTC");
        j.times = Some(3);
        j.runs_done = 2;
        assert!(!j.is_finished(utc(1, 0, 0)));
        assert_eq!(j.runs_left(), Some(1));
        j.runs_done = 3;
        assert!(j.is_finished(utc(1, 0, 0)));
        assert_eq!(j.runs_left(), Some(0));
        assert_eq!(j.due(utc(1, 0, 5), utc(0, 0, 0)).unwrap(), None);
    }

    #[test]
    fn pause_skips_without_replay_and_resumes() {
        let since = utc(0, 0, 0);
        let mut j = job("*/10 * * * *", "UTC");
        j.paused_until = Some(utc(3, 35, 0).timestamp());
        assert!(j.is_paused(utc(3, 20, 5)) && !j.is_paused(utc(3, 35, 0)));
        assert_eq!(j.due(utc(3, 20, 5), since).unwrap(), None);
        assert_eq!(j.skipped(utc(3, 20, 5), since).unwrap(), Some(utc(3, 20, 0)));
        j.last_run = Some(utc(3, 20, 0).timestamp());
        assert_eq!(j.skipped(utc(3, 20, 40), since).unwrap(), None, "recorded once");
        // After the pause: the 03:30 slot (skipped) is not replayed, 03:40 runs.
        j.last_run = Some(utc(3, 30, 0).timestamp());
        assert_eq!(j.due(utc(3, 36, 0), since).unwrap(), None);
        assert_eq!(j.due(utc(3, 40, 5), since).unwrap(), Some(utc(3, 40, 0)));
        assert_eq!(j.next_run(utc(3, 20, 5)).unwrap(), Some(utc(3, 40, 0)), "next run after the pause");
    }

    #[test]
    fn json_roundtrip_of_limits() {
        let mut j = job("* * * * *", "UTC");
        j.times = Some(5);
        j.runs_done = 2;
        j.until = Some(100);
        let back: Job = serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap();
        assert_eq!(back, j);
        let plain = serde_json::to_value(job("* * * * *", "UTC")).unwrap();
        assert!(plain.get("times").is_none() && plain.get("runs_done").is_none() && plain.get("paused_until").is_none());
    }

    #[test]
    fn created_by_is_optional_and_roundtrips() {
        let legacy = r#"[{"id":"snapreport","cron":"*/5 * * * *","tz":"Asia/Jakarta","enabled":true,"kind":"command","command":"x","last_run":1793358600,"runs_done":4,"until":1793361600}]"#;
        let s: Schedule = serde_json::from_str(legacy).unwrap();
        assert!(s.0[0].created_by.is_none());
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back, serde_json::from_str::<serde_json::Value>(legacy).unwrap(), "legacy content unchanged");
        let mut j = job("* * * * *", "UTC");
        j.created_by = Some(CreatedBy {
            name: "bo".into(),
            backend: Some(BackendKind::Herdr),
            pane_id: Some("w1:p2".into()),
            origin: JobOrigin::Session,
            chat_id: None,
        });
        let back: Job = serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap();
        assert_eq!(back, j);
        assert_eq!(j.created_by.unwrap().label(), "by bo via session");
    }
}
