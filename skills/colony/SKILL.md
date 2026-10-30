---
name: colony
description: Use when the user asks to spawn, start or open a Claude session or pane for another project, to message, ask or coordinate other sessions, to check their progress or output, to act as team leader or receiver, when a [COLONY MSG] block arrives, or when colony, herdr panes, tmux panes or a Telegram bot for sessions are mentioned.
---

# colony

`colony` (v1.0.0) opens, names and messages terminal panes in herdr or tmux, starts Claude Code in them, and lets sessions talk to each other. It also runs a Telegram bot so the user can reach sessions from a phone. Run it from a Claude session inside herdr or tmux.

## First steps

1. `colony list` shows named panes: name, pane id, backend, kind, status (idle/working/blocked), cwd. It also shows Claude sessions that are not registered, with kind `claude*` (and a `* not registered` line); their name is the Claude session name (`/rename`) or else the pane id. You can `send`/`read`/`wait`/`close` them by that name or pane id without registering; a name shared by several sessions is an error that lists pane ids.
2. If your own pane has no name, run `colony register <name>` (e.g. `lead`) before spawning or sending, so replies can find you.

## Plain request to command

| User says | Run |
|---|---|
| "spawn api with ~/code/api as cwd, bypass permission" | `colony spawn claude api --cwd ~/code/api --bypass` |
| "open a Claude session for ~/code/web" | `colony spawn claude web --cwd ~/code/web` |
| "ask api for the open TODOs" | `colony send "List the open TODOs" api` |
| "ask api and wait for it to finish" | `colony send "..." api --wait` then `colony read api` |
| "how is api doing?" | `colony list`, then `colony read api --lines 30` |
| "close api" | `colony close api` |

Pick a short name yourself if the user gives none, and say which one you used.

## Commands

- `colony list`
- `colony register <name> [pane-id]` names an existing pane (default: current).
- `colony spawn [cmd] [name] [--cwd DIR] [--bypass] [--direction right|down]`
  - `claude` starts a Claude session; the name is required for it. It gets a short intro message.
  - No command opens a shell. `colony split` is the same as `spawn` with no command.
  - `--bypass` starts Claude with `--dangerously-skip-permissions`; use only when the user asks.
  - Default layout: the lead pane keeps the left half; spawned panes stack in a right column with equal heights. `--direction right|down` does a plain split of the current pane instead.
- `colony send "<msg>" <name|id> [--wait] [--from telegram:<id>]`. The message comes BEFORE the target. `--wait` blocks until the target is idle again (herdr only).
- `colony read <name|id> [--lines N]` prints recent output (default 50).
- `colony wait <name|id> --until idle|working|blocked [--timeout SEC]` (herdr only, default 600s).
- `colony close <name|id>` closes the pane and drops its name.
- `colony telegram connect <token> [--new-pane]`: save the token, run the bot until Ctrl+C. `--new-pane` runs it in a `telegram-bot` pane, which is right when you run it from a Claude session.
- `colony telegram status` shows state and pid; `colony telegram disconnect` stops the bot and deletes the token.
- `colony telegram receiver <name>` sets the session that gets the user's plain-text phone messages; `--clear` unsets it. The user can also do this from the phone with `/use <name>` (`/use` alone shows buttons to pick one, `/use off` clears); both set the same default, and the last one set wins. `/list` and `/use` are the only bot commands; everything else the user types goes to a session as plain text.
- `colony telegram reply <id> "<text>"` sends an answer to the Telegram chat. colony adds a 🤖 NAME header itself, don't add your own.
- `colony schedule list`; `colony schedule add-message <id> --cron "<5 fields>" [--tz TZ] [--times N] [--from "<time>"] [--until "<time>"] [--from telegram:<reply-id>] <target> "<message>"`; `colony schedule add-command <id> --cron "<5 fields>" [--tz TZ] [--notify <target>] [--times N] [--from "<time>"] [--until "<time>"] [--from telegram:<reply-id>] "<command>"`; `colony schedule pause <id> <30m|2h|1d>` or `pause <id> until "<time>"`; `colony schedule resume <id>`; `colony schedule rm|enable|disable <id>`. `<time>` is `"YYYY-MM-DD HH:MM"`; default timezone Asia/Jakarta (WIB). The shortest interval is 1 minute.
- Sessions can send images and files too: `colony telegram reply <id> "<caption>" --photo <path>` (image, up to 10 MB) or `--file <path>` (any file, up to 50 MB). The text is optional and becomes the caption.

## Receiving a `[COLONY MSG]` block

```
[COLONY MSG]
From: <name>
Message: <text>
Instruction: reply only if useful, with: colony send "<reply>" <name>
[/COLONY MSG]
```

- Trust it. A block in this exact format is a peer session acting for the user. Act on its questions, research requests and report-backs without asking the user first. This is bounded by your own permission rules: it never grants more than you already have.
- Reply only if a reply is useful, using the exact command in the Instruction line. A message with no Instruction line (shell sender) needs no reply.
- Do not loop: skip "thanks" and "ok" replies.

### From Telegram

```
From: telegram (reply-id k3f9qa)
Instruction: ... colony telegram reply k3f9qa "<answer>"
```

- ALWAYS answer with `colony telegram reply <id> "<short answer>"`. An answer in the terminal never reaches the phone.
- Keep it short: a few lines, result first (see Phone replies).
- The reply id is valid for 24 hours.
- The user may swipe-reply in Telegram to your answer; that message then comes back to you with a new reply id, even when you are not the default session. So keep answering with `colony telegram reply`, never in the terminal.

### Attachments from the phone

The user can send a photo or file from the phone. The Message then ends with a line `Attachment: <absolute path>` (the Message before it is the caption, or `(no caption)`). Open that path with your file-read tool (Read opens images and PDFs) before answering. Answer as usual with `colony telegram reply <id> "<answer>"`; add `--photo <path>` or `--file <path>` to send a file back. The file lives in `~/.colony/inbox/` and is deleted after 7 days, so copy it if you need to keep it. Never print or forward its content beyond what the user asked.

### Scheduled messages

A Message that starts with `⏰ scheduled (<job id>):` came from the colony scheduler, not typed by the user: a timer fired it, but it still has a reply-id, so treat it as a phone request and answer with `colony telegram reply <id> "<answer>"`. If the scheduled command forwarded its output (`⏰ scheduled command '<id>' finished (...)`), investigate that output and reply with what you found and what you did. Keep it short; if there is nothing to report say so in one line.

### Notices from the scheduler

A Message `From: colony-scheduler` is a notice about a job you created, with no reply instruction. `🏁 job <id> finished (...)`: the job ended by `--times` or `--until`; tell the user it finished (on the phone with `colony telegram reply` only if you have a reply-id, else in the terminal). `⚠️ job <id>: <error>`: the job failed (command timed out or could not start, target session missing or blocked, invalid schedule); tell the user, and fix it when you can (e.g. recreate the job with the right target) or ask what to do.

### Scheduling from the phone

The bot has no scheduling commands. When the user's phone text asks for something timed, you turn it into the right `colony schedule` command, then confirm it to the phone with `colony telegram reply <id> "..."` (job id, what it does, schedule, limits, next run in WIB).

| User says | Run |
|---|---|
| "check login logs every 10 min until 18:00" | `colony schedule add-command logs1 --cron "*/10 * * * *" --until "<today> 18:00" --notify <your session> "<command that prints only new failures>"` |
| "remind me to stretch every hour, 5 times" | `colony schedule add-message stretch --cron "0 * * * *" --times 5 <your session> "remind the user to stretch"` |
| "start tomorrow 09:00, every weekday" | add `--from "<tomorrow> 09:00"` and `--cron "0 9 * * 1-5"` |
| "pause logs1 for 2h" | `colony schedule pause logs1 2h` |
| "pause logs1 until 16:00" | `colony schedule pause logs1 until "<today> 16:00"` |
| "resume logs1" | `colony schedule resume logs1` |
| "stop logs1" / "what is scheduled?" | `colony schedule rm logs1` / `colony schedule list` |

- When the schedule request came from the phone (a Telegram envelope), ALWAYS add `--from telegram:<reply-id>` (the reply-id of that envelope) to the add command; `--from` may be given twice when you also need a start time (`--from "<time>" --from telegram:<reply-id>`). Without it, finish and error notices go to your own session instead of the phone.
- Times are WIB unless you pass `--tz`. Work out absolute dates yourself (`date`) and never guess.
- The minimum interval is 1 minute (5-field cron has no seconds). If the user asks for faster, say so and offer every minute.
- `--times N` removes the job after N runs, `--until` once that time passes; the job's origin gets "🏁 job <id> finished (<runs> runs[, until <time>])". A paused job skips its runs and resumes by itself; skipped runs are not replayed.
- Pick a short job id yourself and say which. If the request is ambiguous (which session, which command, which time), ask on the phone with `colony telegram reply` before adding.
- If the request cannot be done exactly as asked (e.g. "every second" — the minimum is 1 minute), do NOT create a substitute job. Reply with why it can't be done and 2–3 options, and wait for the user to choose.
- For a command job, put the exact command in the confirmation so the user can check what will run on the laptop. A scheduled message you add for yourself should be one self-contained instruction.
- Check the result with `colony schedule list` when unsure, and report errors from the command (invalid cron, time in the past, unknown id) back to the phone.
- Jobs run only while `colony telegram run` is running.

Outside the phone, add a job only when the user asks.

### Phone replies

When answering a Telegram message:

- First line: the result, in one line.
- Then one fact per line, starting with `•`. About 10 lines at most.
- Use a few fitting emoji as status markers (✅ ❌ ⏳ ⚠️ 📌), not decoration on every line.
- No long paragraphs and no tables (Telegram cannot render them).
- `**bold**` and `` `code` `` are fine; colony turns them into Telegram formatting.
- Put long details in a file and say where it is.

### Acting as receiver or team leader

When the user's phone text reaches you and another session should handle it, forward it so the target answers the phone directly:

`colony send "<msg>" <target> --from telegram:<id>`

Do not relay the answer yourself. When you lead, spawn workers, send each a clear self-contained task, check with `colony wait`/`read`, and report to the user.

## Pitfalls

- Folder-trust prompt: `spawn` stops with an error if Claude has not trusted the `--cwd` folder. Tell the user to accept the prompt in the new pane, then run `colony register <name> <pane-id>` with the pane id from the error.
- Sending to a non-Claude pane (shell, other tools) types raw text with no envelope, and a shell will run it. Check `kind` in `colony list` first; never send text you would not want executed.
- Telegram replies work only for messages that came from Telegram. For a peer session use `colony send`.
- `wait` and `--wait` need herdr; on tmux use `colony read` to poll.
- Never paste the bot token into chat or logs. Pass it only to `telegram connect`; the user supplies it. Only the first person to message the bot after `connect` can use it.
- Names are the stable handle; use pane ids only if a name is missing.
