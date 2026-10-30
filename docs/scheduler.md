# Scheduler

The scheduler runs **inside the bot loop only** (`colony telegram run`). When the bot is stopped nothing runs, and jobs that were due meanwhile are **not** replayed. Cron parsing and next/previous fire times use the [`croner`](https://crates.io/crates/croner) crate (4.0.2); see "Crate choice" below.

## schedule.json

`~/.colony/schedule.json` (mode 600, written atomically under the state lock) is a list of jobs:

```json
[
  {"id": "morning", "cron": "0 9 * * 1-5", "tz": "Asia/Jakarta", "enabled": true,
   "kind": "message", "target": "bo", "message": "status of the build?"},
  {"id": "disk", "cron": "*/30 * * * *", "tz": "Asia/Jakarta", "enabled": true,
   "kind": "command", "command": "df -h / | awk 'NR==2 && $5+0>90'", "notify_target": "bo",
   "last_run": 1788000000}
]
```

- `cron`: exactly 5 fields (minute hour day-of-month month day-of-week); validated on add. Seconds are refused.
- `tz`: IANA name, default `Asia/Jakarta`; validated on add. The cron fields are in this timezone (DST aware).
- `last_run`: unix time of the scheduled minute that fired last (set by the bot).
- `command` jobs may also have `timeout_secs` (default 120).
- `created_by` (optional, absent in older files; they keep working unchanged): `name`, `backend`, `pane_id`, `origin` (`session`, `telegram` or `none`) and `chat_id` (see "Job origin").
- Limits (all optional): `times` (job removes itself after that many runs) with `runs_done` (runs so far), `from` and `until` (unix times), `paused_until` (unix time).

## Kinds

**message**: at each fire the bot creates a new Telegram reply id and sends `⏰ scheduled (<id>): <message>` to the target session with the Telegram envelope, like a message typed on the phone, so the session answers on the phone with `colony telegram reply`. If nobody has locked the bot yet, the job is skipped (logged on stderr). If the session is missing or blocked, the phone gets a short notice; a busy session gets the message queued as usual.

**command**: the command runs with `sh -c` in `$HOME`, 120 s timeout (the process group is killed). If the exit status is not 0, the command timed out, or stdout is not empty, the locked chat gets a message: `⏰ <id>`, the exit code (or "timed out"), then the output in a code block (plain text inside `<pre>`; the tail is kept when it is too long; stderr is appended when present). Exit 0 with empty stdout is silent. With `notify_target`, the same output is also forwarded to that session with the Telegram envelope and a new reply id, so it can investigate and answer on the phone. A command runs inside the bot loop, so the bot does not read Telegram while a command runs (at most its timeout).

## Timing

After every poll the loop checks, at most every ~30 s, which enabled jobs are due. A job is due when its latest scheduled minute (computed in its timezone) is newer than `last_run` and not older than the bot start. The minute is recorded in `last_run` before the job runs, so a scheduled minute fires at most once, also across restarts.

## Limits and pause

Every time below is `"YYYY-MM-DD HH:MM"`, in WIB (Asia/Jakarta) unless the job has another `--tz`. They work on `add-message` and `add-command`:

- `--times N`: the job removes itself after N runs (N at least 1).
- `--until "<time>"`: the job removes itself once that time is passed. There is no run at or after it. It must lie in the future and after `--from`.
- `--from "<time>"`: no runs before that time.

`colony schedule pause <id> <duration>` (`30m`, `2h`, `1d`; 1 minute to 365 days) or `colony schedule pause <id> until "<time>"` (the time is WIB, or the zone given with `--tz`) skips the runs while paused and resumes by itself afterwards. `colony schedule resume <id>` ends the pause early. **Skipped runs are not replayed**: the scheduled minutes that pass while paused are recorded as done, so nothing fires late when the pause ends.

When a job removes itself (`--times` reached after its last run, or `--until` passed) its origin gets "🏁 job <id> finished (<runs> runs[, until <time WIB>])" (see "Job origin"). An `--until` that passes while the bot is stopped is handled at the next start: the job is removed and the notice is sent then. A run only counts for `--times` when it can really go out (a locked chat exists).

The shortest interval is 1 minute (5-field cron has no seconds).

## Job origin

Every job added with `add-message` / `add-command` records who created it (`created_by`): the session of the caller's pane (registered name, else the discovered `/rename` name, else the pane id, plus backend and pane id) and an origin channel.

- `--from telegram:<reply-id>` (same format as `colony send --from`): origin = telegram. The reply id's chat is stored. The id must be valid (24 h). `--from` already means "start time" here, so a job needing both repeats it: `--from "2026-10-30 09:00" --from telegram:k3f9qa`.
- Without it, origin = the creating session.
- No pane detected and no `--from`: origin = telegram if a chat is locked, else none.

Notices go back to the origin:

- `🏁 job <id> finished (<runs> runs[, until <time WIB>])` when `--times` or `--until` ends the job.
- `⚠️ job <id>: <short error>` when a command job times out or cannot start, a message job's target is missing or blocked, or the stored cron/timezone is invalid (reported once per bot run).
- Origin telegram (or none, or a job without `created_by`): sent to the stored chat, else the locked chat.
- Origin session: delivered to that session as a `[COLONY MSG]` from `colony-scheduler` with no reply instruction. If the session no longer exists, the notice goes to Telegram instead. A command timeout is also shown in the normal command alert on Telegram.

Normal command output alerts are unchanged (locked chat). `colony schedule list` shows `by <name> via <telegram|session>` per job. Old jobs without `created_by` behave as before (Telegram) and are never rewritten by the scheduler except for their own `last_run` / `runs_done`.

## CLI

```
colony schedule list
colony schedule add-message <id> --cron "<expr>" [--tz TZ] [--times N] [--from "<time>"] [--until "<time>"] [--from telegram:<reply-id>] <target> "<message>"
colony schedule add-command <id> --cron "<expr>" [--tz TZ] [--notify <target>] [--times N] [--from "<time>"] [--until "<time>"] [--from telegram:<reply-id>] "<command>"
colony schedule pause <id> <30m|2h|1d>
colony schedule pause <id> until "<time>" [--tz TZ]
colony schedule resume <id>
colony schedule rm <id>
colony schedule enable <id>
colony schedule disable <id>
```

`list` shows each job with its next run in WIB (after `--from`, after an active pause, and none when `--until` comes first), the limits (`runs 3/10 (7 left)`, `from`, `until`) and `paused until` while a pause is active. Editing the file does not need a bot restart: it is read at every tick.

## Phone

The bot has no scheduling commands. Scheduling from the phone happens by typing normally to a session: it turns requests such as "check login logs every 10 min until 18:00" or "pause logs1 for 2h" into the matching `colony schedule` command and confirms the job back with `colony telegram reply` (see the colony skill). The bot only knows `/list` and `/use`.

## Built-in maintenance job

Not in the file: the inbox cleanup (see [telegram.md](telegram.md)) runs at bot start and every hour, deleting files older than 7 days from `~/.colony/inbox`.

## Crate choice

`croner` 4.0.2 (released 2026-10-21) over `cron` 0.17.0: it parses the standard 5-field form natively (the `cron` crate needs a seconds field), supports `chrono` time zones through `chrono-tz` (already a dependency), has a previous-occurrence search (used to find the scheduled minute to fire), and is actively maintained.
