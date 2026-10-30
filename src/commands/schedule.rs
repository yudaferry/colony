//! `colony schedule ...`: edit `schedule.json`. Jobs run only while the bot is running.

use crate::backend::detect_env;
use crate::cli::{LimitArgs, Origin};
use crate::ctx::Ctx;
use crate::discover::discover;
use crate::state::names::{NAMES_FILE, Names};
use crate::state::pending::{PENDING_FILE, Pending};
use crate::state::schedule::{
    CreatedBy, JobKind, JobOrigin, Limits, SCHEDULE_FILE, Schedule, add_job_with, parse_duration, parse_local,
    pause_job, remove_job, resume_job, set_enabled,
};
use crate::state::telegram::{TELEGRAM_FILE, TelegramConfig};
use crate::time::wib;
use anyhow::{Result, bail};

pub fn list(ctx: &Ctx) -> Result<String> {
    let s: Schedule = ctx.state.read_json(SCHEDULE_FILE)?;
    if s.0.is_empty() {
        return Ok("no scheduled jobs\n".to_string());
    }
    let now = ctx.clock.now();
    let at = |t: i64| chrono::DateTime::from_timestamp(t, 0).map_or_else(|| "?".to_string(), wib);
    let mut out = String::new();
    for j in &s.0 {
        let what = match &j.kind {
            JobKind::Message { target, message } => format!("message -> {target}: {message}"),
            JobKind::Command { command, notify_target, .. } => match notify_target {
                Some(t) => format!("command (notify {t}): {command}"),
                None => format!("command: {command}"),
            },
        };
        let next = match (j.enabled, j.next_run(now)) {
            (false, _) => "-".to_string(),
            (true, Ok(Some(n))) => wib(n),
            (true, Ok(None)) => "none".to_string(),
            (true, Err(_)) => "?".to_string(),
        };
        let mut extra = String::new();
        match (j.times, j.runs_left()) {
            (Some(t), Some(left)) => extra.push_str(&format!("  runs {}/{t} ({left} left)", j.runs_done)),
            _ if j.runs_done > 0 => extra.push_str(&format!("  runs {}", j.runs_done)),
            _ => {}
        }
        if let Some(f) = j.from {
            extra.push_str(&format!("  from {}", at(f)));
        }
        if let Some(u) = j.until {
            extra.push_str(&format!("  until {}", at(u)));
        }
        if let (true, Some(p)) = (j.is_paused(now), j.paused_until) {
            extra.push_str(&format!("  paused until {}", at(p)));
        }
        if let Some(c) = &j.created_by {
            extra.push_str(&format!("  {}", c.label()));
        }
        out.push_str(&format!(
            "{}  {}  \"{}\" {}  next {}{}  {}\n",
            j.id,
            if j.enabled { "enabled" } else { "disabled" },
            j.cron,
            j.tz,
            next,
            extra,
            what
        ));
    }
    out.push_str("(jobs run only while `colony telegram run` is running; times are WIB)\n");
    Ok(out)
}

/// Turns `--times/--from/--until` into limits. Times are in `tz`; `--until` must lie in the future.
fn limits_of(ctx: &Ctx, tz: &str, a: &LimitArgs) -> Result<Limits> {
    let from = a.start().map_err(anyhow::Error::msg)?.map(|t| parse_local(t, tz)).transpose()?;
    let until = a.until.as_deref().map(|t| parse_local(t, tz)).transpose()?;
    if until.is_some_and(|u| u <= ctx.clock.now().timestamp()) {
        bail!("--until is already in the past");
    }
    Ok(Limits { times: a.times, from, until })
}

/// The session that runs `colony schedule add-*`: the caller's pane, named by its registered
/// name, else its discovered (/rename) name, else the pane id.
fn caller_session(ctx: &Ctx) -> Option<CreatedBy> {
    let backend = detect_env(ctx.env).ok()?;
    let b = ctx.backends.get(backend);
    let pane = b.current_pane().ok()?;
    let names: Names = ctx.state.read_json(NAMES_FILE).unwrap_or_default();
    let name = names.name_of_pane(backend, &pane).map(str::to_string).or_else(|| {
        let panes = b.list_panes().ok()?;
        discover(ctx, backend, &panes, &names).into_iter().find(|d| d.pane_id == pane).map(|d| d.name)
    });
    Some(CreatedBy {
        name: name.unwrap_or_else(|| pane.clone()),
        backend: Some(backend),
        pane_id: Some(pane),
        origin: JobOrigin::Session,
        chat_id: None,
    })
}

/// Who created the job and where its notices go (see docs/scheduler.md).
fn creator(ctx: &Ctx, a: &LimitArgs) -> Result<CreatedBy> {
    let caller = caller_session(ctx);
    if let Some(Origin::Telegram(id)) = a.origin().map_err(anyhow::Error::msg)? {
        let pending: Pending = ctx.state.read_json(PENDING_FILE)?;
        let Some(entry) = pending.get_valid(&id, ctx.clock.now()) else {
            bail!("reply id {id} unknown or expired (valid 24 h)");
        };
        let mut c = caller.unwrap_or(CreatedBy {
            name: "telegram".to_string(),
            backend: None,
            pane_id: None,
            origin: JobOrigin::Telegram,
            chat_id: None,
        });
        c.origin = JobOrigin::Telegram;
        c.chat_id = Some(entry.chat_id);
        return Ok(c);
    }
    if let Some(c) = caller {
        return Ok(c);
    }
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    let locked = cfg.locked_chat_id;
    Ok(CreatedBy {
        name: "user".to_string(),
        backend: None,
        pane_id: None,
        origin: if locked.is_some() { JobOrigin::Telegram } else { JobOrigin::None },
        chat_id: locked,
    })
}

pub fn add_message(
    ctx: &Ctx,
    id: &str,
    cron: &str,
    tz: &str,
    limits: &LimitArgs,
    target: &str,
    message: &str,
) -> Result<String> {
    let kind = JobKind::Message { target: target.to_string(), message: message.to_string() };
    add_job_with(ctx.state, id, cron, tz, kind, limits_of(ctx, tz, limits)?, Some(creator(ctx, limits)?))?;
    Ok(format!("Added message job {id}"))
}

pub fn add_command(
    ctx: &Ctx,
    id: &str,
    cron: &str,
    tz: &str,
    notify: Option<&str>,
    limits: &LimitArgs,
    command: &str,
) -> Result<String> {
    let kind = JobKind::Command {
        command: command.to_string(),
        notify_target: notify.map(str::to_string),
        timeout_secs: None,
    };
    add_job_with(ctx.state, id, cron, tz, kind, limits_of(ctx, tz, limits)?, Some(creator(ctx, limits)?))?;
    Ok(format!("Added command job {id}"))
}

pub fn rm(ctx: &Ctx, id: &str) -> Result<String> {
    remove_job(ctx.state, id)?;
    Ok(format!("Removed job {id}"))
}

pub fn enable(ctx: &Ctx, id: &str, on: bool) -> Result<String> {
    set_enabled(ctx.state, id, on)?;
    Ok(format!("{} job {id}", if on { "Enabled" } else { "Disabled" }))
}

/// `pause <id> <duration>` or `pause <id> until "<YYYY-MM-DD HH:MM>"` (time in `tz`).
pub fn pause(ctx: &Ctx, id: &str, when: &[String], tz: &str) -> Result<String> {
    let now = ctx.clock.now();
    let until = match when {
        [d] => (now + parse_duration(d)?).timestamp(),
        [kw, t] if kw.eq_ignore_ascii_case("until") => parse_local(t, tz)?,
        _ => bail!("usage: colony schedule pause <id> <30m|2h|1d>  or  pause <id> until \"YYYY-MM-DD HH:MM\""),
    };
    if until <= now.timestamp() {
        bail!("the pause end is already in the past");
    }
    pause_job(ctx.state, id, until)?;
    let end = chrono::DateTime::from_timestamp(until, 0).map_or_else(|| "?".to_string(), wib);
    Ok(format!("Paused job {id} until {end}"))
}

pub fn resume(ctx: &Ctx, id: &str) -> Result<String> {
    Ok(if resume_job(ctx.state, id, ctx.clock.now())? {
        format!("Resumed job {id}")
    } else {
        format!("Job {id} is not paused")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testutil::Harness;
    use crate::state::schedule::Job;
    use std::os::unix::fs::PermissionsExt;

    fn none() -> LimitArgs {
        LimitArgs::default()
    }

    #[test]
    fn add_list_disable_enable_rm() {
        let h = Harness::herdr();
        let c = h.ctx();
        assert_eq!(list(&c).unwrap(), "no scheduled jobs\n");
        add_message(&c, "m", "0 9 * * *", "Asia/Jakarta", &none(), "bo", "status?").unwrap();
        add_command(&c, "d", "*/5 * * * *", "UTC", Some("bo"), &none(), "df -h").unwrap();
        let mode = std::fs::metadata(h.state.path(SCHEDULE_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let out = list(&c).unwrap();
        // clock 2026-10-29 14:52:03Z = 21:52 WIB
        assert!(out.contains("m  enabled  \"0 9 * * *\" Asia/Jakarta  next 2026-10-30 09:00 WIB  by w1:p1 via session  message -> bo: status?"), "{out}");
        assert!(out.contains("d  enabled  \"*/5 * * * *\" UTC  next 2026-10-29 21:55 WIB  by w1:p1 via session  command (notify bo): df -h"), "{out}");
        disable_check(&c);
    }

    fn disable_check(c: &Ctx) {
        assert_eq!(enable(c, "m", false).unwrap(), "Disabled job m");
        assert!(list(c).unwrap().contains("m  disabled"));
        assert_eq!(enable(c, "m", true).unwrap(), "Enabled job m");
        assert_eq!(rm(c, "m").unwrap(), "Removed job m");
        assert!(rm(c, "m").is_err());
        assert!(enable(c, "m", true).is_err());
    }

    #[test]
    fn validation_errors_write_nothing() {
        let h = Harness::herdr();
        let c = h.ctx();
        assert!(add_message(&c, "a", "bad cron", "UTC", &none(), "bo", "x").unwrap_err().to_string().contains("5 fields"));
        assert!(add_message(&c, "a", "0 9 * * *", "Mars/Base", &none(), "bo", "x").unwrap_err().to_string().contains("timezone"));
        assert!(add_message(&c, "bad id!", "0 9 * * *", "UTC", &none(), "bo", "x").is_err());
        assert!(add_command(&c, "a", "0 9 * * *", "UTC", None, &none(), "  ").is_err());
        assert!(add_command(&c, "a", "70 9 * * *", "UTC", None, &none(), "ls").is_err());
        assert_eq!(list(&c).unwrap(), "no scheduled jobs\n");
        add_command(&c, "a", "0 9 * * *", "UTC", None, &none(), "ls").unwrap();
        assert!(add_message(&c, "a", "0 9 * * *", "UTC", &none(), "bo", "x").unwrap_err().to_string().contains("already exists"));
    }

    fn limits(times: Option<u32>, from: Option<&str>, until: Option<&str>) -> LimitArgs {
        LimitArgs { times, from: from.map(String::from).into_iter().collect(), until: until.map(String::from) }
    }

    fn job_of(c: &Ctx, id: &str) -> Job {
        c.state.read_json::<Schedule>(SCHEDULE_FILE).unwrap().get(id).unwrap().clone()
    }

    // clock: 2026-10-29 14:52:03Z = 21:52 WIB

    #[test]
    fn limits_are_stored_in_the_job_timezone_and_listed() {
        let h = Harness::herdr();
        let c = h.ctx();
        add_message(&c, "m", "*/10 * * * *", "Asia/Jakarta", &limits(Some(10), Some("2026-10-29 22:00"), Some("2026-10-30 18:00")), "bo", "logs?").unwrap();
        let j = job_of(&c, "m");
        assert_eq!(j.times, Some(10));
        assert_eq!(j.from, Some(parse_local("2026-10-29 22:00", "Asia/Jakarta").unwrap()));
        assert_eq!(j.until, Some(parse_local("2026-10-30 18:00", "Asia/Jakarta").unwrap()));
        let out = list(&c).unwrap();
        // first slot is 22:00 WIB (from), 10 runs left
        assert!(out.contains("next 2026-10-29 22:00 WIB  runs 0/10 (10 left)  from 2026-10-29 22:00 WIB  until 2026-10-30 18:00 WIB  by w1:p1 via session  message -> bo: logs?"), "{out}");
        // --tz applies to from/until too: 18:00 UTC = 01:00 WIB next day
        add_command(&c, "u", "0 * * * *", "UTC", None, &limits(None, None, Some("2026-10-30 18:00")), "df").unwrap();
        assert!(list(&c).unwrap().contains("until 2026-10-31 01:00 WIB"));
    }

    #[test]
    fn limit_validation() {
        let h = Harness::herdr();
        let c = h.ctx();
        let add = |l: LimitArgs| add_message(&c, "a", "* * * * *", "UTC", &l, "bo", "x");
        assert!(add(limits(Some(0), None, None)).unwrap_err().to_string().contains("at least 1"));
        assert!(add(limits(None, Some("tomorrow"), None)).is_err());
        assert!(add(limits(None, None, Some("2026-10-30"))).is_err());
        assert!(add(limits(None, None, Some("2026-10-29 10:00"))).unwrap_err().to_string().contains("past"));
        assert!(add(limits(None, Some("2026-10-31 10:00"), Some("2026-10-30 10:00"))).unwrap_err().to_string().contains("after --from"));
        assert_eq!(list(&c).unwrap(), "no scheduled jobs\n");
        assert!(add(limits(Some(1), None, Some("2026-10-30 10:00"))).is_ok());
    }

    #[test]
    fn pause_and_resume_commands() {
        let h = Harness::herdr();
        let c = h.ctx();
        add_message(&c, "m", "0 9 * * *", "Asia/Jakarta", &none(), "bo", "x").unwrap();
        assert_eq!(pause(&c, "m", &["2h".into()], "Asia/Jakarta").unwrap(), "Paused job m until 2026-10-29 23:52 WIB");
        assert_eq!(job_of(&c, "m").paused_until, Some(h.clock.0.timestamp() + 7200));
        let out = list(&c).unwrap();
        assert!(out.contains("paused until 2026-10-29 23:52 WIB"), "{out}");
        assert_eq!(pause(&c, "m", &["until".into(), "2026-10-30 10:00".into()], "Asia/Jakarta").unwrap(), "Paused job m until 2026-10-30 10:00 WIB");
        // paused over the 09:00 slot: next run is the one after the pause
        assert!(list(&c).unwrap().contains("next 2026-10-31 09:00 WIB"));
        assert_eq!(resume(&c, "m").unwrap(), "Resumed job m");
        assert_eq!(job_of(&c, "m").paused_until, None);
        assert!(!list(&c).unwrap().contains("paused"));
        assert!(list(&c).unwrap().contains("next 2026-10-30 09:00 WIB"));
        assert_eq!(resume(&c, "m").unwrap(), "Job m is not paused");
        // validation
        for bad in [vec!["0m".to_string()], vec!["soon".into()], vec!["until".into()], vec!["until".into(), "x".into()], vec!["until".into(), "2026-10-29 10:00".into()], vec!["a".into(), "b".into(), "c".into()]] {
            assert!(pause(&c, "m", &bad, "Asia/Jakarta").is_err(), "{bad:?}");
        }
        assert!(pause(&c, "ghost", &["1h".into()], "UTC").is_err());
        assert!(resume(&c, "ghost").is_err());
    }

    use crate::backend::{AgentState, BackendKind, Kind};
    use crate::state::pending::PendingEntry;
    use crate::state::telegram::{TELEGRAM_MODE};

    fn with_pending(h: &Harness, id: &str, chat: i64) {
        h.state
            .update_json(PENDING_FILE, 0o600, |p: &mut Pending| {
                p.0.insert(id.into(), PendingEntry { target: "lead".into(), chat_id: chat, message_id: 1, created_at: h.clock.0 });
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn created_by_is_the_caller_pane_registered_then_discovered_then_pane_id() {
        // registered
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p1", Some(("claude", AgentState::Idle)));
        h.add_name("lead", BackendKind::Herdr, "w1:p1", Kind::Claude);
        add_message(&h.ctx(), "m", "0 9 * * *", "UTC", &none(), "bo", "x").unwrap();
        let c = job_of(&h.ctx(), "m").created_by.unwrap();
        assert_eq!((c.name.as_str(), c.backend, c.pane_id.as_deref(), c.origin), ("lead", Some(BackendKind::Herdr), Some("w1:p1"), JobOrigin::Session));
        assert!(list(&h.ctx()).unwrap().contains("by lead via session"));
        // discovered /rename name
        let h = Harness::herdr();
        h.backends.herdr.add_claude("w1:p1", AgentState::Idle, Some("renamed"));
        add_command(&h.ctx(), "c", "0 9 * * *", "UTC", None, &none(), "df").unwrap();
        assert_eq!(job_of(&h.ctx(), "c").created_by.unwrap().name, "renamed");
        // plain pane id
        let h = Harness::herdr();
        h.backends.herdr.add_pane("w1:p1", None);
        add_command(&h.ctx(), "c", "0 9 * * *", "UTC", None, &none(), "df").unwrap();
        assert_eq!(job_of(&h.ctx(), "c").created_by.unwrap().name, "w1:p1");
    }

    #[test]
    fn from_telegram_stores_the_reply_chat() {
        let h = Harness::herdr();
        h.add_name("lead", BackendKind::Herdr, "w1:p1", Kind::Claude);
        with_pending(&h, "k3f9qa", 77);
        let l = LimitArgs { from: vec!["telegram:k3f9qa".into(), "2026-10-29 22:00".into()], ..none() };
        add_message(&h.ctx(), "m", "0 9 * * *", "UTC", &l, "bo", "x").unwrap();
        let j = job_of(&h.ctx(), "m");
        let c = j.created_by.unwrap();
        assert_eq!((c.name.as_str(), c.origin, c.chat_id), ("lead", JobOrigin::Telegram, Some(77)));
        assert!(j.from.is_some(), "the time --from still works beside the origin");
        assert!(list(&h.ctx()).unwrap().contains("by lead via telegram"));
        let bad = LimitArgs { from: vec!["telegram:nope".into()], ..none() };
        assert!(add_message(&h.ctx(), "m2", "0 9 * * *", "UTC", &bad, "bo", "x").unwrap_err().to_string().contains("unknown or expired"));
        assert!(list(&h.ctx()).unwrap().lines().count() <= 3);
    }

    #[test]
    fn no_pane_and_no_from_is_telegram_when_a_chat_is_locked_else_none() {
        let h = Harness::new(&[]);
        add_command(&h.ctx(), "a", "0 9 * * *", "UTC", None, &none(), "df").unwrap();
        assert_eq!(job_of(&h.ctx(), "a").created_by.unwrap().origin, JobOrigin::None);
        h.state
            .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                c.locked_chat_id = Some(42);
                Ok(())
            })
            .unwrap();
        add_command(&h.ctx(), "b", "0 9 * * *", "UTC", None, &none(), "df").unwrap();
        let c = job_of(&h.ctx(), "b").created_by.unwrap();
        assert_eq!((c.origin, c.chat_id), (JobOrigin::Telegram, Some(42)));
    }

    #[test]
    fn legacy_schedule_json_loads_lists_and_is_not_rewritten_by_reads() {
        let h = Harness::herdr();
        let legacy = r#"[{"id":"snapreport","cron":"*/5 * * * *","tz":"Asia/Jakarta","enabled":true,"kind":"command","command":"x","last_run":1793358600,"runs_done":4,"until":1793361600}]"#;
        std::fs::write(h.state.path(SCHEDULE_FILE), legacy).unwrap();
        let out = list(&h.ctx()).unwrap();
        assert!(out.contains("snapreport") && !out.contains(" by "), "{out}");
        assert_eq!(std::fs::read_to_string(h.state.path(SCHEDULE_FILE)).unwrap(), legacy);
        add_message(&h.ctx(), "new", "0 9 * * *", "UTC", &none(), "bo", "x").unwrap();
        let s: Schedule = h.state.read_json(SCHEDULE_FILE).unwrap();
        let v = serde_json::to_value(&s.0[0]).unwrap();
        assert_eq!(v, serde_json::from_str::<serde_json::Value>(legacy).unwrap()[0], "old job untouched");
    }

    #[test]
    fn from_flag_accepts_time_and_origin_and_refuses_duplicates() {
        let l = LimitArgs { from: vec!["telegram:a".into(), "2026-10-30 09:00".into()], ..none() };
        assert_eq!(l.start().unwrap(), Some("2026-10-30 09:00"));
        assert!(l.origin().unwrap().is_some());
        assert!(LimitArgs { from: vec!["a".into(), "b".into()], ..none() }.start().is_err());
        assert!(LimitArgs { from: vec!["telegram:a".into(), "telegram:b".into()], ..none() }.origin().is_err());
        assert!(LimitArgs { from: vec!["telegram:".into()], ..none() }.origin().is_err());
    }
}
