# Architecture

colony is one binary. The same executable is the command-line tool that you and your Claude Code sessions run, and the Telegram bot (`colony telegram run`). There is no daemon other than the bot, no web server and no public URL.

```
 Telegram ──► colony telegram run (bot pane)
                 │ slash command ──► colony core ──► backend (herdr | tmux) ──► target pane
                 │ plain text ─────► receiver session (optional) ── colony send ──┘
 target session ── colony telegram reply <id> "<text>" ──► Telegram Bot API (direct)
```

## Backends

All pane operations go through a `Backend` trait (`src/backend/mod.rs`) with two implementations, `Herdr` and `Tmux`. One process can drive both at the same time.

The trait covers: current pane, list panes, split (with direction and cwd), the spawn-column split (see [layout](layout.md)), run a program in a pane, start Claude in a pane, name an agent, send text and submit, read recent output, wait for an agent state, close a pane, agent status and the pane's foreground program.

Both implementations call the `herdr` or `tmux` executable through a small `Runner` trait, so tests can record the exact arguments instead of starting a multiplexer.

### Which backend is used

| Command | Backend |
|---|---|
| `send`, `read`, `wait`, `close` on a **named** target | the backend stored for that name in `names.json`, not the caller's environment |
| `spawn`, `split`, `register`, `telegram connect` | the caller's environment |

The first rule is what lets the bot (running in a herdr pane) reach a session that lives in tmux, and the other way round.

For the second rule, colony looks at the environment variables of the calling process:

1. `TMUX` or `TMUX_PANE` set: **tmux**. tmux wins because it is the innermost multiplexer; a tmux started from a herdr pane still inherits the `HERDR_*` variables.
2. `HERDR_ENV=1` or `HERDR_PANE_ID` set: **herdr**.
3. Otherwise the command fails with an error that names the variables it looked for.

A target argument is matched against names first. A raw pane id (`%3` for tmux, `w1:p2` for herdr) selects the backend from its shape and takes the kind of the name that points at it, if any.

### herdr and tmux differences

- **Status.** `colony list` shows idle / working / blocked on herdr. tmux has no agent status, so it shows a dash.
- **`wait` and `send --wait`.** herdr only. On tmux the message is still delivered, then colony reports "herdr only".
- **Claude panes on herdr** are started as herdr agents (`herdr agent start`), and the colony name is also the herdr agent name, so `agent prompt`, `agent read` and `agent wait` work. Other panes are addressed by pane id (`pane run`, `pane read`).
- **tmux** uses `split-window`, `send-keys`/`paste-buffer`, `capture-pane`, `respawn-pane` and `kill-pane`.

## State

State lives in `~/.colony/` (created with mode 700):

| File | Content | Mode |
|---|---|---|
| `names.json` | name -> backend, pane id, kind (claude / shell / other), cwd, created time, `spawned_by` | 644 |
| `telegram.json` | bot token, locked user and chat id, receiver name, connect time, polling offset and the bot it belongs to | 600 |
| `pending.json` | reply id -> target, Telegram chat and message to answer, created time | 600 |
| `colony.log` | one line per send, reply, spawn, close and register, with timestamps | - |

Names are stored lower-case. Entries whose pane no longer exists are dropped by `colony list`.

### Atomic, locked writes

Every change to a JSON state file is a read-modify-write under one exclusive lock on `~/.colony/.lock` (`StateDir::update_json`). The new content is written to `<file>.tmp.<pid>` with the final mode, fsynced and renamed over the target. If the update closure fails, nothing is written. This keeps the bot and any number of CLI calls from losing each other's changes; a test runs four threads doing 25 increments each and checks that none are lost.

## Module map

| Path | Role |
|---|---|
| `src/main.rs` | entry point: parse arguments, build the context, dispatch |
| `src/cli.rs` | `clap` command definitions and the `--from telegram:<id>` origin |
| `src/ctx.rs` | `Ctx` (state, backends, env, clock) and the `Clock` / `Env` traits, with fixed and map-based fakes |
| `src/target.rs` | resolves a name or pane id to a target |
| `src/envelope.rs` | message envelopes and the spawn intro ([messaging](messaging.md)) |
| `src/time.rs` | timestamps shown to the user and in the log (UTC+7, labelled WIB) |
| `src/backend/` | `Backend` trait, `herdr.rs`, `tmux.rs`, `runner.rs` (process runner and a recording fake), `fake.rs` (in-memory backend) |
| `src/commands/` | one file per command: `list`, `register`, `spawn` (also `split`), `send`, `read`, `wait`, `close` |
| `src/state/` | `store.rs` (locked atomic JSON), `names.rs`, `pending.rs`, `telegram.rs`, `log.rs` |
| `src/telegram/` | `api.rs` (Bot API client), `bot.rs` (update handler and polling loop), `cmds.rs` (`connect`, `disconnect`, `status`, `receiver`, `reply`), `reply_id.rs`, `split.rs` ([telegram](telegram.md)) |

Commands are plain functions that take a `Ctx`, so each can be tested with fake backends, a fixed clock and a temporary state directory ([development](development.md)).
