//! The scheduler. It runs only inside the bot loop (`colony telegram run`): `tick` is
//! called about every 30 s, fires the jobs of `schedule.json` that are due, and
//! never fires the same scheduled minute twice. Jobs missed while the bot was
//! stopped are not replayed.

use super::api::TelegramApi;
use super::bot::deliver_scheduled;
use super::format::escape_html;
use super::split::{TG_LIMIT, tail_fit};
use crate::ctx::Ctx;
use crate::backend::{Kind, PaneRef};
use crate::envelope;
use crate::state::schedule::{Job, JobKind, JobOrigin, SCHEDULE_FILE, SCHEDULE_MODE, Schedule};
use crate::state::telegram::{TELEGRAM_FILE, TelegramConfig};
use crate::time::{wib, wib_secs};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the bot loop checks for due jobs.
pub const TICK_SECS: i64 = 30;
pub const ICON_SCHED: &str = "\u{23f0}";
const ICON_DONE: &str = "\u{1f3c1}";
const ICON_WARN: &str = "\u{26a0}\u{fe0f}";
/// Sender of the notices a job sends to its creating session.
pub const SCHEDULER_NAME: &str = "colony-scheduler";
const MAX_CAPTURE: u64 = 1024 * 1024;


/// What a command job produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdResult {
    /// Exit code; `None` when it was killed (timeout) or never started.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// The command could not be started.
    pub error: Option<String>,
}

/// Runs a shell command; faked in tests.
pub trait CmdRunner {
    fn run(&self, command: &str, timeout: Duration, cwd: &Path) -> CmdResult;
}

/// `sh -c <command>` in `cwd`, killed (with its process group) after `timeout`.
pub struct ShellRunner;

fn capture<R: Read + Send + 'static>(r: R) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&buf);
    let h = std::thread::spawn(move || {
        let mut r = r.take(MAX_CAPTURE);
        let mut chunk = [0u8; 8192];
        while let Ok(n) = r.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if let Ok(mut g) = b.lock() {
                g.extend_from_slice(&chunk[..n]);
            }
        }
    });
    (buf, h)
}

fn text_of(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    buf.lock().map(|g| String::from_utf8_lossy(&g).into_owned()).unwrap_or_default()
}

impl CmdRunner for ShellRunner {
    fn run(&self, command: &str, timeout: Duration, cwd: &Path) -> CmdResult {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let spawned = Command::new("sh")
            .args(["-c", command])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => return CmdResult { error: Some(e.to_string()), ..Default::default() },
        };
        let (out, out_h) = capture(child.stdout.take().expect("piped stdout"));
        let (err, err_h) = capture(child.stderr.take().expect("piped stderr"));
        let start = Instant::now();
        let (code, timed_out) = loop {
            match child.try_wait() {
                Ok(Some(st)) => break (st.code(), false),
                Ok(None) if start.elapsed() >= timeout => {
                    // Kill the whole group so no grandchild keeps the pipes open.
                    let _ = Command::new("kill").args(["-KILL", &format!("-{}", child.id())]).status();
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break (None, false),
            }
        };
        // A background grandchild can hold a pipe open: give the readers a moment, no more.
        let grace = Instant::now();
        while !(out_h.is_finished() && err_h.is_finished()) && grace.elapsed() < Duration::from_millis(300) {
            std::thread::sleep(Duration::from_millis(10));
        }
        CmdResult { code, stdout: text_of(&out), stderr: text_of(&err), timed_out, error: None }
    }
}

/// Sends a job notice (finished, error) to the origin of the job: the creating session as a
/// [COLONY MSG] from `colony-scheduler` (no reply instruction), else, or when that session no
/// longer exists, the Telegram chat (the stored one, else the locked one).
fn notice(ctx: &Ctx, api: &dyn TelegramApi, locked: Option<i64>, job: &Job, text: &str) {
    let by = job.created_by.as_ref();
    if let Some(c) = by.filter(|c| c.origin == JobOrigin::Session) {
        if let (Some(backend), Some(pane)) = (c.backend, c.pane_id.as_deref()) {
            let b = ctx.backends.get(backend);
            let alive = b.list_panes().is_ok_and(|ps| ps.iter().any(|p| p.pane_id == pane));
            if alive {
                let target = PaneRef { backend, pane_id: pane.to_string(), name: Some(c.name.clone()), kind: Kind::Claude };
                if b.send(&target, &envelope::laptop(Some(SCHEDULER_NAME), None, text)).is_ok() {
                    return;
                }
            }
        }
    }
    let chat = by.filter(|c| c.origin == JobOrigin::Telegram).and_then(|c| c.chat_id).or(locked);
    match chat {
        Some(chat) => {
            let _ = api.send_message(chat, text, None);
        }
        None => eprintln!("colony telegram: job {}: no place to send: {text}", job.id),
    }
}

/// "🏁 job <id> finished (<runs> runs[, until <time WIB>])"
fn finished_text(job: &Job, runs: u32) -> String {
    let until = job
        .until
        .and_then(|u| DateTime::from_timestamp(u, 0))
        .map(|u| format!(", until {}", wib(u)))
        .unwrap_or_default();
    format!("{ICON_DONE} job {} finished ({runs} runs{until})", job.id)
}

fn error_text(job: &str, short: &str) -> String {
    format!("{ICON_WARN} job {job}: {short}")
}

/// Jobs with an invalid cron/timezone already reported during this bot run (the tick is every 30 s).
fn first_invalid_report(id: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default).lock().map(|mut g| g.insert(id.to_string())).unwrap_or(false)
}

/// Fires every due job. `since` is when the bot started: older scheduled minutes are never run.
/// Also: jobs past `--until` are removed, scheduled minutes that pass while a job is paused are
/// skipped (recorded, never replayed), and a job that reached `--times` is removed after its
/// last run. Each removal sends a "finished" notice to the job's origin.
pub fn tick(ctx: &Ctx, api: &dyn TelegramApi, runner: &dyn CmdRunner, since: DateTime<Utc>) -> Result<()> {
    let now = ctx.clock.now();
    let schedule: Schedule = ctx.state.read_json(SCHEDULE_FILE)?;
    let mut due: Vec<(Job, DateTime<Utc>)> = Vec::new();
    let mut skipped: Vec<(String, DateTime<Utc>)> = Vec::new();
    let mut expired: Vec<Job> = Vec::new();
    let mut invalid: Vec<(Job, String)> = Vec::new();
    for j in &schedule.0 {
        if j.is_finished(now) {
            expired.push(j.clone());
            continue;
        }
        match j.due(now, since) {
            Ok(Some(minute)) => due.push((j.clone(), minute)),
            Ok(None) => {}
            Err(e) => {
                eprintln!("[{}] colony telegram: job {}: {e:#}", wib_secs(now), j.id);
                if first_invalid_report(&j.id) {
                    invalid.push((j.clone(), format!("invalid schedule: {e:#}")));
                }
            }
        }
        if let Ok(Some(minute)) = j.skipped(now, since) {
            skipped.push((j.id.clone(), minute));
        }
    }
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    let chat = cfg.locked_chat_id;
    for (j, short) in &invalid {
        notice(ctx, api, chat, j, &error_text(&j.id, short));
    }
    if due.is_empty() && skipped.is_empty() && expired.is_empty() {
        return Ok(());
    }
    // Record the minute before running: a crash or a slow command can never fire it twice.
    // A run only counts (for --times) when it can really go out. Finished jobs leave the file.
    let mut done_after_run: Vec<String> = Vec::new();
    let expired_ids: Vec<String> = expired.iter().map(|j| j.id.clone()).collect();
    ctx.state.update_json(SCHEDULE_FILE, SCHEDULE_MODE, |s: &mut Schedule| {
        for (j, minute) in &due {
            if let Some(x) = s.get_mut(&j.id) {
                x.last_run = Some(minute.timestamp());
                if chat.is_some() {
                    x.runs_done += 1;
                    if x.times.is_some_and(|t| x.runs_done >= t) {
                        done_after_run.push(x.id.clone());
                    }
                }
            }
        }
        for (id, minute) in &skipped {
            if let Some(x) = s.get_mut(id) {
                x.last_run = Some(minute.timestamp());
            }
        }
        s.0.retain(|x| !expired_ids.contains(&x.id) && !done_after_run.contains(&x.id));
        Ok(())
    })?;
    let Some(chat) = chat else {
        for (j, _) in &due {
            eprintln!("[{}] colony telegram: job {} skipped: no locked chat", wib_secs(now), j.id);
        }
        for j in &expired {
            notice(ctx, api, None, j, &finished_text(j, j.runs_done));
        }
        return Ok(());
    };
    for (j, minute) in &due {
        let r = match &j.kind {
            JobKind::Message { target, message } => run_message(ctx, api, chat, j, target, message),
            JobKind::Command { command, notify_target, .. } => {
                run_command(ctx, api, runner, chat, j, command, notify_target.as_deref())
            }
        };
        if let Err(e) = r {
            eprintln!("[{}] colony telegram: job {} ({}) failed: {e:#}", wib_secs(now), j.id, wib(*minute));
        }
    }
    for (j, _) in due.iter().filter(|(j, _)| done_after_run.contains(&j.id)) {
        notice(ctx, api, Some(chat), j, &finished_text(j, j.runs_done + 1));
    }
    for j in &expired {
        notice(ctx, api, Some(chat), j, &finished_text(j, j.runs_done));
    }
    Ok(())
}

/// Delivers `text` to a session and tells the phone only when that did not work.
fn forward_or_report(ctx: &Ctx, api: &dyn TelegramApi, chat: i64, job: &Job, target: &str, text: &str) -> Result<()> {
    let ack = deliver_scheduled(ctx, chat, target, text)?;
    if !ack.starts_with(ICON_SCHED) {
        let short = ack.trim_start_matches(|c: char| !c.is_alphanumeric());
        notice(ctx, api, Some(chat), job, &error_text(&job.id, short));
    }
    Ok(())
}

fn run_message(ctx: &Ctx, api: &dyn TelegramApi, chat: i64, job: &Job, target: &str, message: &str) -> Result<()> {
    let text = format!("{ICON_SCHED} scheduled ({}): {message}", job.id);
    forward_or_report(ctx, api, chat, job, target, &text)
}

/// Shrinks the tail of `output` until `build(output)` fits one Telegram message.
fn fit_output(output: &str, build: impl Fn(&str) -> String) -> String {
    let mut limit = TG_LIMIT;
    loop {
        let out = build(&tail_fit(output, limit));
        if out.encode_utf16().count() <= TG_LIMIT || limit <= 16 {
            return out;
        }
        limit /= 2;
    }
}

fn run_command(
    ctx: &Ctx,
    api: &dyn TelegramApi,
    runner: &dyn CmdRunner,
    chat: i64,
    job: &Job,
    command: &str,
    notify: Option<&str>,
) -> Result<()> {
    let home = ctx.env.var("HOME").unwrap_or_else(|| "/".to_string());
    let timeout = job.timeout_secs();
    let res = runner.run(command, Duration::from_secs(timeout), Path::new(&home));
    // The alert below already tells Telegram; a creating session gets the short notice.
    let to_session = job.created_by.as_ref().is_some_and(|c| c.origin == JobOrigin::Session);
    if to_session && (res.error.is_some() || res.timed_out) {
        let short = match &res.error {
            Some(e) => format!("could not start: {e}"),
            None => format!("timed out after {timeout} s"),
        };
        notice(ctx, api, Some(chat), job, &error_text(&job.id, &short));
    }
    let failed = res.error.is_some() || res.timed_out || res.code != Some(0);
    if !failed && res.stdout.trim().is_empty() {
        return Ok(());
    }
    let status = if let Some(e) = &res.error {
        format!("could not start: {e}")
    } else if res.timed_out {
        format!("timed out after {timeout} s")
    } else {
        match res.code {
            Some(c) => format!("exit code {c}"),
            None => "killed by a signal".to_string(),
        }
    };
    let mut output = res.stdout.trim_end().to_string();
    if !res.stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(res.stderr.trim_end());
    }
    if output.is_empty() {
        output = "(no output)".to_string();
    }
    let header = format!("{ICON_SCHED} {}\n{status}", job.id);
    let html = fit_output(&output, |o| format!("<b>{}</b>\n{}\n<pre>{}</pre>", escape_html(&format!("{ICON_SCHED} {}", job.id)), escape_html(&status), escape_html(o)));
    if api.send_html(chat, &html, None).is_err() {
        let plain = fit_output(&output, |o| format!("{header}\n{o}"));
        api.send_message(chat, &plain, None)?;
    }
    if let Some(target) = notify {
        let text = format!(
            "{ICON_SCHED} scheduled command '{}' finished ({status}). Output:\n{}",
            job.id,
            tail_fit(&output, 3500)
        );
        forward_or_report(ctx, api, chat, job, target, &text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AgentState, BackendKind, Kind};
    use crate::commands::testutil::Harness;
    use crate::state::pending::{PENDING_FILE, Pending};
    use crate::state::telegram::TELEGRAM_MODE;
    use crate::state::schedule::{add_job, set_enabled};
    use crate::telegram::api::FakeTelegram;
    use chrono::TimeZone;
    use std::cell::RefCell;

    struct FakeRunner {
        result: CmdResult,
        calls: RefCell<Vec<(String, Duration, String)>>,
    }

    impl FakeRunner {
        fn new(code: i32, stdout: &str) -> Self {
            Self {
                result: CmdResult { code: Some(code), stdout: stdout.to_string(), ..Default::default() },
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CmdRunner for FakeRunner {
        fn run(&self, command: &str, timeout: Duration, cwd: &Path) -> CmdResult {
            self.calls.borrow_mut().push((command.to_string(), timeout, cwd.display().to_string()));
            self.result.clone()
        }
    }

    fn since() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, 14, 0, 0).unwrap()
    }

    fn locked(h: &Harness) {
        h.state
            .update_json(TELEGRAM_FILE, TELEGRAM_MODE, |c: &mut TelegramConfig| {
                c.token = Some("111:SECRET".into());
                c.locked_user_id = Some(42);
                c.locked_chat_id = Some(42);
                Ok(())
            })
            .unwrap();
    }

    fn session(h: &Harness, name: &str, pane: &str, st: AgentState) {
        h.backends.herdr.add_pane(pane, Some(("claude", st)));
        h.add_name(name, BackendKind::Herdr, pane, Kind::Claude);
    }

    fn add_msg(h: &Harness, id: &str) {
        add_job(&h.state, id, "* * * * *", "UTC", JobKind::Message { target: "bo".into(), message: "status?".into() }).unwrap();
    }

    fn add_cmd(h: &Harness, id: &str, notify: Option<&str>) {
        add_job(
            &h.state,
            id,
            "* * * * *",
            "UTC",
            JobKind::Command { command: "df".into(), notify_target: notify.map(String::from), timeout_secs: None },
        )
        .unwrap();
    }

    fn schedule(h: &Harness) -> Schedule {
        h.state.read_json(SCHEDULE_FILE).unwrap()
    }

    #[test]
    fn message_job_sends_envelope_and_creates_pending_once() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "daily");
        let api = FakeTelegram::new();
        let runner = FakeRunner::new(0, "");
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        let sends = h.backends.herdr.sends();
        assert_eq!(sends.len(), 1, "{sends:?}");
        assert!(sends[0].starts_with("send w1:p2 [COLONY MSG]\nFrom: telegram (reply-id "), "{sends:?}");
        assert!(sends[0].contains("scheduled (daily): status?"), "{sends:?}");
        let p: Pending = h.state.read_json(PENDING_FILE).unwrap();
        assert_eq!(p.0.len(), 1);
        let e = p.0.values().next().unwrap();
        assert_eq!((e.target.as_str(), e.chat_id, e.message_id), ("bo", 42, 0));
        assert!(api.sent.borrow().is_empty(), "no noise on success");
        // Same minute, later tick: no second fire.
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
        assert_eq!(schedule(&h).get("daily").unwrap().last_run, Some(h.clock.0.timestamp() - 3));
    }

    #[test]
    fn disabled_job_and_jobs_before_start_do_not_fire() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "a");
        set_enabled(&h.state, "a", false).unwrap();
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty());
        set_enabled(&h.state, "a", true).unwrap();
        // The bot started after this minute: missed, not replayed.
        let late = h.clock.0;
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), late).unwrap();
        assert!(h.backends.herdr.sends().is_empty());
    }

    fn set_job(h: &Harness, id: &str, f: impl Fn(&mut Job)) {
        h.state
            .update_json(SCHEDULE_FILE, SCHEDULE_MODE, |s: &mut Schedule| {
                f(s.get_mut(id).unwrap());
                Ok(())
            })
            .unwrap();
    }

    fn finished_notices(api: &FakeTelegram) -> Vec<String> {
        api.sent.borrow().iter().map(|m| m.1.clone()).filter(|t| t.contains("finished")).collect()
    }

    #[test]
    fn times_removes_the_job_after_its_last_run_and_tells_the_phone() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "t");
        set_job(&h, "t", |j| j.times = Some(2));
        let api = FakeTelegram::new();
        let runner = FakeRunner::new(0, "");
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
        assert_eq!(schedule(&h).get("t").unwrap().runs_done, 1);
        assert!(finished_notices(&api).is_empty());
        // next minute: second and last run, then the job is gone
        let mut h2 = h.ctx();
        let later = crate::ctx::FixedClock(h.clock.0 + chrono::Duration::minutes(1));
        h2.clock = &later;
        tick(&h2, &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 2);
        assert!(schedule(&h).get("t").is_none(), "removed after the last run");
        assert_eq!(finished_notices(&api), vec!["\u{1f3c1} job t finished (2 runs)".to_string()]);
        // nothing more
        tick(&h2, &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 2);
    }

    #[test]
    fn times_one_command_job_runs_once_then_finishes() {
        let h = Harness::herdr();
        locked(&h);
        add_cmd(&h, "c", None);
        set_job(&h, "c", |j| j.times = Some(1));
        let api = FakeTelegram::new();
        let runner = FakeRunner::new(0, "");
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert_eq!(runner.calls.borrow().len(), 1);
        assert!(schedule(&h).0.is_empty());
        assert_eq!(finished_notices(&api).len(), 1);
    }

    #[test]
    fn until_removes_the_job_once_passed_without_a_run() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "u");
        let now = h.clock.0.timestamp();
        // still ahead: it runs and stays
        set_job(&h, "u", |j| j.until = Some(now + 3600));
        let api = FakeTelegram::new();
        let runner = FakeRunner::new(0, "");
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
        assert!(schedule(&h).get("u").is_some() && finished_notices(&api).is_empty());
        // reached: removed, no run at or after it
        set_job(&h, "u", |j| j.until = Some(now));
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
        assert!(schedule(&h).get("u").is_none());
        assert_eq!(finished_notices(&api), vec!["\u{1f3c1} job u finished (1 runs, until 2026-10-29 21:52 WIB)".to_string()]);
    }

    #[test]
    fn from_blocks_runs_until_reached() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "f");
        let now = h.clock.0.timestamp();
        set_job(&h, "f", |j| j.from = Some(now + 600));
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty());
        assert_eq!(schedule(&h).get("f").unwrap().runs_done, 0);
        set_job(&h, "f", |j| j.from = Some(now - 600));
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
    }

    #[test]
    fn paused_job_skips_without_replay_and_resumes_by_itself() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "p");
        let now = h.clock.0;
        // pause until 2 minutes from now
        set_job(&h, "p", |j| j.paused_until = Some(now.timestamp() + 120));
        let api = FakeTelegram::new();
        let runner = FakeRunner::new(0, "");
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty(), "skipped while paused");
        let j = schedule(&h).get("p").unwrap().clone();
        assert_eq!((j.runs_done, j.last_run.is_some()), (0, true), "skipped minute recorded, not counted");
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty());
        // 1 minute in: still paused, new minute skipped
        let c1 = crate::ctx::FixedClock(now + chrono::Duration::minutes(1));
        let mut ctx1 = h.ctx();
        ctx1.clock = &c1;
        tick(&ctx1, &api, &runner, since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty());
        // pause over: the next minute runs, nothing is replayed
        let c2 = crate::ctx::FixedClock(now + chrono::Duration::minutes(2));
        let mut ctx2 = h.ctx();
        ctx2.clock = &c2;
        tick(&ctx2, &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
        tick(&ctx2, &api, &runner, since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1, "no second fire of the same minute");
        assert_eq!(schedule(&h).get("p").unwrap().runs_done, 1);
    }

    #[test]
    fn resume_early_does_not_replay_the_skipped_minute() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "r");
        let now = h.clock.0;
        crate::state::schedule::pause_job(&h.state, "r", now.timestamp() + 3600).unwrap();
        // resume before any tick saw the minute
        assert!(crate::state::schedule::resume_job(&h.state, "r", now).unwrap());
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty(), "the minute passed during the pause");
        let c = crate::ctx::FixedClock(now + chrono::Duration::minutes(1));
        let mut ctx = h.ctx();
        ctx.clock = &c;
        tick(&ctx, &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert_eq!(h.backends.herdr.sends().len(), 1);
    }

    #[test]
    fn no_locked_chat_skips() {
        let h = Harness::herdr();
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_msg(&h, "a");
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty());
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn message_job_to_missing_or_blocked_session_tells_the_phone() {
        let h = Harness::herdr();
        locked(&h);
        add_msg(&h, "a");
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert!(api.sent.borrow()[0].1.contains("no session named bo"), "{:?}", api.sent.borrow());
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn command_silent_on_exit0_empty_output() {
        let h = Harness::herdr();
        locked(&h);
        add_cmd(&h, "ok", None);
        let api = FakeTelegram::new();
        let runner = FakeRunner::new(0, "  \n");
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert_eq!(runner.calls.borrow().len(), 1);
        assert_eq!(runner.calls.borrow()[0].1, Duration::from_secs(120));
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn command_alerts_on_output_or_nonzero() {
        for (code, out, want) in [(0, "disk 91% <full>", "disk 91% &lt;full&gt;"), (2, "", "exit code 2")] {
            let h = Harness::herdr();
            locked(&h);
            add_cmd(&h, "disk", None);
            let api = FakeTelegram::new();
            tick(&h.ctx(), &api, &FakeRunner::new(code, out), since()).unwrap();
            let html = api.html_sent.borrow();
            assert_eq!(html.len(), 1);
            assert_eq!(html[0].0, 42);
            assert!(html[0].1.contains("\u{23f0} disk"), "{}", html[0].1);
            assert!(html[0].1.contains("<pre>"), "{}", html[0].1);
            assert!(html[0].1.contains(want), "{}", html[0].1);
        }
    }

    #[test]
    fn command_timeout_alerts() {
        let h = Harness::herdr();
        locked(&h);
        add_cmd(&h, "slow", None);
        let api = FakeTelegram::new();
        let runner = FakeRunner {
            result: CmdResult { timed_out: true, ..Default::default() },
            calls: RefCell::new(Vec::new()),
        };
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        assert!(api.html_sent.borrow()[0].1.contains("timed out after 120 s"));
    }

    #[test]
    fn long_output_is_trimmed_to_fit() {
        let h = Harness::herdr();
        locked(&h);
        add_cmd(&h, "big", None);
        let api = FakeTelegram::new();
        let out = "a&b\n".repeat(5000);
        tick(&h.ctx(), &api, &FakeRunner::new(1, &out), since()).unwrap();
        let html = api.html_sent.borrow();
        assert!(html[0].1.encode_utf16().count() <= TG_LIMIT);
        assert!(html[0].1.ends_with("</pre>"));
    }

    #[test]
    fn html_rejected_falls_back_to_plain() {
        let h = Harness::herdr();
        locked(&h);
        add_cmd(&h, "x", None);
        let api = FakeTelegram::new();
        api.reject_html.set(true);
        tick(&h.ctx(), &api, &FakeRunner::new(1, "boom"), since()).unwrap();
        assert_eq!(api.sent.borrow()[0].1, "\u{23f0} x\nexit code 1\nboom");
    }

    #[test]
    fn notify_target_forwards_output_to_session() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "bo", "w1:p2", AgentState::Idle);
        add_cmd(&h, "disk", Some("bo"));
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(1, "disk full"), since()).unwrap();
        assert_eq!(api.html_sent.borrow().len(), 1);
        let sends = h.backends.herdr.sends();
        assert_eq!(sends.len(), 1);
        assert!(sends[0].contains("From: telegram (reply-id "), "{sends:?}");
        assert!(sends[0].contains("disk full") && sends[0].contains("'disk'"), "{sends:?}");
        let p: Pending = h.state.read_json(PENDING_FILE).unwrap();
        assert_eq!(p.0.len(), 1);
        // Silent run: nothing forwarded.
        let h2 = Harness::herdr();
        locked(&h2);
        session(&h2, "bo", "w1:p2", AgentState::Idle);
        add_cmd(&h2, "disk", Some("bo"));
        tick(&h2.ctx(), &FakeTelegram::new(), &FakeRunner::new(0, ""), since()).unwrap();
        assert!(h2.backends.herdr.sends().is_empty());
    }

    fn created(origin: JobOrigin, pane: &str, chat: Option<i64>) -> crate::state::schedule::CreatedBy {
        crate::state::schedule::CreatedBy {
            name: "lead".into(),
            backend: Some(crate::backend::BackendKind::Herdr),
            pane_id: Some(pane.into()),
            origin,
            chat_id: chat,
        }
    }

    fn texts(api: &FakeTelegram) -> Vec<(i64, String)> {
        api.sent.borrow().iter().map(|m| (m.0, m.1.clone())).collect()
    }

    #[test]
    fn finish_notice_goes_to_the_creating_session_without_reply_instruction() {
        let h = Harness::herdr();
        locked(&h);
        session(&h, "lead", "w1:p5", AgentState::Idle);
        add_cmd(&h, "c", None);
        set_job(&h, "c", |j| {
            j.times = Some(1);
            j.created_by = Some(created(JobOrigin::Session, "w1:p5", None));
        });
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        let sends = h.backends.herdr.sends();
        assert_eq!(sends.len(), 1, "{sends:?}");
        assert_eq!(
            sends[0],
            format!("send w1:p5 {}", envelope::laptop(Some("colony-scheduler"), None, "\u{1f3c1} job c finished (1 runs)"))
        );
        assert!(!sends[0].contains("Instruction"));
        assert!(texts(&api).is_empty(), "nothing on Telegram");
    }

    #[test]
    fn finish_notice_goes_to_telegram_for_telegram_origin_and_stored_chat() {
        let h = Harness::herdr();
        locked(&h);
        add_cmd(&h, "c", None);
        set_job(&h, "c", |j| {
            j.times = Some(1);
            j.created_by = Some(created(JobOrigin::Telegram, "w1:p5", Some(99)));
        });
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert_eq!(texts(&api), vec![(99, "\u{1f3c1} job c finished (1 runs)".to_string())]);
        assert!(h.backends.herdr.sends().is_empty());
    }

    #[test]
    fn session_gone_falls_back_to_telegram() {
        let h = Harness::herdr();
        locked(&h);
        add_cmd(&h, "c", None);
        set_job(&h, "c", |j| {
            j.times = Some(1);
            j.created_by = Some(created(JobOrigin::Session, "w1:p77", None));
        });
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        assert!(h.backends.herdr.sends().is_empty());
        assert_eq!(texts(&api), vec![(42, "\u{1f3c1} job c finished (1 runs)".to_string())]);
    }

    #[test]
    fn errors_go_to_the_session_origin() {
        // command timeout
        let h = Harness::herdr();
        locked(&h);
        session(&h, "lead", "w1:p5", AgentState::Idle);
        add_cmd(&h, "slow", None);
        set_job(&h, "slow", |j| j.created_by = Some(created(JobOrigin::Session, "w1:p5", None)));
        let runner = FakeRunner { result: CmdResult { timed_out: true, ..Default::default() }, calls: RefCell::new(Vec::new()) };
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &runner, since()).unwrap();
        let sends = h.backends.herdr.sends();
        assert_eq!(sends.len(), 1, "{sends:?}");
        assert!(sends[0].contains("From: colony-scheduler") && sends[0].contains("\u{26a0}\u{fe0f} job slow: timed out after 120 s"), "{sends:?}");
        // message job whose target is missing
        let h = Harness::herdr();
        locked(&h);
        session(&h, "lead", "w1:p5", AgentState::Idle);
        add_msg(&h, "m");
        set_job(&h, "m", |j| j.created_by = Some(created(JobOrigin::Session, "w1:p5", None)));
        let api = FakeTelegram::new();
        tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
        let sends = h.backends.herdr.sends();
        assert_eq!(sends.len(), 1, "{sends:?}");
        assert!(sends[0].contains("\u{26a0}\u{fe0f} job m: no session named bo"), "{sends:?}");
        assert!(texts(&api).is_empty());
    }

    #[test]
    fn errors_go_to_telegram_for_telegram_and_legacy_jobs() {
        for created_by in [None, Some(created(JobOrigin::Telegram, "w1:p5", None))] {
            let h = Harness::herdr();
            locked(&h);
            add_msg(&h, "m");
            set_job(&h, "m", |j| j.created_by = created_by.clone());
            let api = FakeTelegram::new();
            tick(&h.ctx(), &api, &FakeRunner::new(0, ""), since()).unwrap();
            assert_eq!(texts(&api), vec![(42, "\u{26a0}\u{fe0f} job m: no session named bo".to_string())]);
        }
    }

    #[test]
    fn shell_runner_runs_times_out_and_reports_exit() {
        let r = ShellRunner.run("echo hi; echo err >&2; exit 3", Duration::from_secs(10), Path::new("/"));
        assert_eq!((r.code, r.stdout.as_str(), r.stderr.as_str()), (Some(3), "hi\n", "err\n"));
        let t = Instant::now();
        let r = ShellRunner.run("sleep 30 & sleep 30", Duration::from_millis(300), Path::new("/"));
        assert!(r.timed_out && r.code.is_none());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
