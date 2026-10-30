# colony

Drive several Claude Code sessions from one place — at your desk through a lead Claude session, and away from it through a Telegram bot.

colony is a single command-line tool. It opens, names and messages terminal panes in **[herdr](https://herdr.dev)** or **tmux**, starts Claude Code in them, and lets the sessions talk to each other. A built-in Telegram bot lets you list sessions, message them and read their answers from your phone.

## Features

- **herdr and tmux** — one binary works in both, and can reach sessions in either.
- **Spawn and name sessions** — `colony spawn claude api --cwd ~/code/api` opens a pane, starts Claude, names it and sends it a short intro.
- **Session-to-session messages** — `colony send` wraps the text in a `[COLONY MSG]` envelope that tells the receiver who sent it and how to reply.
- **Read and wait** — show a session's recent output, or wait until it is idle (herdr).
- **Tidy layout** — the lead pane keeps the left half; spawned panes stack in a right column with equal heights.
- **Telegram bot** — `/list` and `/use` (tap a button to pick the default session) from your phone, then just type to talk to that session, or swipe-reply to a session's answer to talk to that same session; sessions answer with `colony telegram reply`, which goes straight to the chat under a `🤖 NAME` header that colony adds itself. Only you can use the bot.
- **No Claude hooks** — sessions reply by running a colony command.

## Install

### Download (Linux x86_64)

Get the latest `colony-<version>-x86_64-linux.tar.gz` from the [Releases](../../releases) page (a static build, with a `.sha256` file):

```bash
tar xzf colony-<version>-x86_64-linux.tar.gz
./colony --help
```

Move `colony` somewhere on your `PATH`, e.g. `~/.local/bin/`.

### Build from source (Nix)

```bash
nix build            # result/bin/colony
nix develop          # shell with Rust 1.99.1, then: cargo build --release
```

Without Nix, use Rust 1.99.1 (see `rust-toolchain.toml`) and run `cargo build --release`.

### Claude Code skill

`skills/colony/SKILL.md` teaches Claude Code sessions how to use colony (spawning, messaging, replying to `[COLONY MSG]` blocks and Telegram). Install it for your user:

```bash
mkdir -p ~/.claude/skills && cp -r skills/colony ~/.claude/skills/
```

## Requirements

- herdr (tested with 0.9.5) and/or tmux
- Claude Code (tested with 2.1.312)
- Linux

## Quick start

From a Claude Code session running inside herdr or tmux:

```bash
colony register lead                                   # name this pane
colony spawn claude api --cwd ~/code/api               # new Claude session "api"
colony send "Summarize the open TODOs" api             # message it
colony read api --lines 30                             # look at its output
colony list                                            # all named panes and their status
colony close api                                       # close it
```

`api` replies by running `colony send "<reply>" lead`, which arrives in the lead session as a `[COLONY MSG]`. You can also just ask the lead Claude in plain words, e.g. "spawn a session for ~/code/api and ask it for the open TODOs".

## Commands

| Command | What it does |
|---|---|
| `colony list` | Name, pane, backend, kind, status (herdr: idle / working / blocked), cwd. Also lists Claude sessions that are not registered, marked `claude*` (see below) |
| `colony register <name> [pane-id]` | Name an existing pane (default: the current one) |
| `colony spawn [<cmd>] [<name>] [--cwd DIR] [--bypass] [--direction right\|down]` | Open a pane; `claude` starts Claude Code; no command opens a shell |
| `colony split` | Same as `colony spawn` with no command |
| `colony send "<msg>" <name\|id> [--wait] [--from telegram:<id>]` | Send a message (envelope for Claude panes, raw text for others) |
| `colony read <name\|id> [--lines N]` | Print recent output |
| `colony wait <name\|id> --until idle\|blocked [--timeout SEC]` | Wait for an agent state (herdr only) |
| `colony close <name\|id>` | Close the pane and drop its name |
| `colony telegram connect <token> [--new-pane]` | Check and save the bot token, run the bot here until Ctrl+C (`--new-pane`: in a `telegram-bot` pane) |
| `colony telegram status` / `disconnect` | Show (incl. running pid) / stop the bot |
| `colony telegram receiver <name>` / `--clear` | Set / clear the session that gets plain-text messages |
| `colony telegram reply <id> "<text>"` | Send an answer straight to the Telegram chat |
| `colony schedule list` / `add-message` / `add-command` / `rm` / `enable` / `disable` / `pause` / `resume` | Manage scheduled jobs, with `--times`, `--from`, `--until` limits (run only while the bot runs) |
| `colony telegram reply <id> ["<text>"] --photo <path>` / `--file <path>` | Send an image (up to 10 MB) or any file (up to 50 MB), the text is its caption |

**Unregistered Claude sessions.** `colony list` and Telegram `/list` also show every Claude Code session the backends can see that has no name in colony, with kind `claude*` and a `* not registered` line. They are shown only; nothing is written to `names.json`. Their name is the Claude session name (set with Claude's `/rename`), or the pane id when none is found. `send`, `read`, `wait`, `close` and `/use` accept that name too: a registered name wins, then a raw pane id, then a unique discovered name (a name shared by several sessions is an error that lists their pane ids). Discovered sessions get the Claude envelope.

`--bypass` starts Claude with `--dangerously-skip-permissions`. In a folder Claude has not trusted yet, `spawn` stops and asks you to accept Claude's prompt in the new pane instead of answering it for you.

## Telegram

1. Create a bot with [@BotFather](https://t.me/BotFather) (`/newbot`) and copy the token.
2. Run `colony telegram connect <token>`. The bot runs in that pane until Ctrl+C (add `--new-pane` inside herdr or tmux to run it in its own `telegram-bot` pane, e.g. from a Claude session).
3. Send any message to your bot. The first person to message it after `connect` is locked in as its only user.

From your phone:

| Message | Effect |
|---|---|
| `/list` | List sessions |
| `/use` | Buttons to pick the default session for plain text (🟢🟡🔴⚪ status, ✅ marks the current one) |
| `/use <name>` / `/use off` | Set or clear the default session by typing (same setting as `colony telegram receiver`; the last one set wins) |
| photo / file | Saved on the laptop (max 20 MB) and sent to the default session (or the swipe-reply target) with an `Attachment:` path line |
| plain text | Goes to the default session (`/use`), if one is set |
| swipe-reply to a session's answer | Goes to the session that sent it, without changing the default (remembered 7 days in `sent.json`) |

The bot has only these two commands and registers them with Telegram each time it starts, so the `/` menu lists just `list` and `use`. Any other `/word` gets "ℹ️ only /list and /use — type normally to talk to your session".

To schedule from the phone, type it normally to a session, for example "check login logs every 10 min until 18:00" or "pause logs1 for 2h"; the session runs the right `colony schedule` command and confirms the job on the phone.

Answers reach Telegram only for messages that came from Telegram.

## Scheduler

`colony schedule` keeps jobs in `~/.colony/schedule.json`; they fire only while `colony telegram run` is running (jobs missed while it was stopped are not replayed). A **message** job sends a message to a session as if you had written it from the phone, so the session answers on Telegram. A **command** job runs a shell command and alerts the phone on a non-zero exit or any output; silent otherwise.

```
colony schedule list
colony schedule add-message morning --cron "0 9 * * 1-5" bo "status of the build?"
colony schedule add-command disk --cron "*/30 * * * *" --notify bo "df -h / | awk 'NR==2 && $5+0>90'"
colony schedule add-command logs --cron "*/10 * * * *" --until "2026-10-30 18:00" --notify bo "grep -c 'login failed' /var/log/auth.log"
colony schedule add-message standup --cron "0 9 * * 1-5" --times 5 --from "2026-11-02 00:00" bo "status?"
colony schedule pause logs 2h          # or: pause logs until "2026-10-30 16:00"
colony schedule resume logs
colony schedule enable|disable|rm <id>
```

`--times N` removes the job after N runs, `--until` removes it once that time is passed, `--from` blocks runs before it. While a job is paused its runs are skipped (not replayed) and it resumes by itself. A job that removes itself sends "🏁 <id> finished" to the phone. `list` shows limits, runs done/left, pause end and the next run. The shortest interval is 1 minute. Times are `"YYYY-MM-DD HH:MM"` in the job's `--tz` (default `Asia/Jakarta`). Details: [Scheduler](docs/scheduler.md).

## Files

State lives in `~/.colony/`:

- `names.json` — pane names
- `telegram.json` — bot token and lock (mode 600)
- `pending.json` — open Telegram reply ids (valid 24 hours)
- `sent.json` — which session sent each Telegram answer, for swipe-replies (mode 600, 7 days)
- `schedule.json` — scheduled jobs (mode 600)
- `inbox/` — photos and files received from the phone (dir 700, files 600, deleted after 7 days)
- `colony.log` — one line per send, reply, spawn, close and register

## Documentation

- [Architecture](docs/architecture.md) — backends, state files, module map
- [Messaging](docs/messaging.md) — envelopes, reply IDs, spawn intro
- [Telegram bot](docs/telegram.md) — lock rules, commands, reply flow, security
- [Scheduler](docs/scheduler.md) — scheduled messages and commands, limits and pause
- [Spawn layout](docs/layout.md) — how panes are arranged on herdr and tmux
- [Development](docs/development.md) — build, tests, CI and releases
- [History and decisions](plan/HISTORY.md) — why colony exists and why it works this way

## Release notes

See [RELEASE_NOTES.md](RELEASE_NOTES.md), including known limitations.
