# colony

Drive several Claude Code sessions from one place — at your desk through a lead Claude session, and away from it through a Telegram bot.

colony is a single command-line tool. It opens, names and messages terminal panes in **[herdr](https://herdr.dev)** or **tmux**, starts Claude Code in them, and lets the sessions talk to each other. A built-in Telegram bot lets you list sessions, message them and read their answers from your phone.

## Features

- **herdr and tmux** — one binary works in both, and can reach sessions in either.
- **Spawn and name sessions** — `colony spawn claude api --cwd ~/code/api` opens a pane, starts Claude, names it and sends it a short intro.
- **Session-to-session messages** — `colony send` wraps the text in a `[COLONY MSG]` envelope that tells the receiver who sent it and how to reply.
- **Read and wait** — show a session's recent output, or wait until it is idle (herdr).
- **Tidy layout** — the lead pane keeps the left half; spawned panes stack in a right column with equal heights.
- **Telegram bot** — `/list`, `/use`, `/send`, `/last` from your phone; sessions answer with `colony telegram reply`, which goes straight to the chat under a `🤖 NAME` header that colony adds itself. Only you can use the bot.
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

**Unregistered Claude sessions.** `colony list` and Telegram `/list` also show every Claude Code session the backends can see that has no name in colony, with kind `claude*` and a `* not registered` line. They are shown only; nothing is written to `names.json`. Their name is the Claude session name (set with Claude's `/rename`), or the pane id when none is found. `send`, `read`, `wait`, `close`, `/send` and `/last` accept that name too: a registered name wins, then a raw pane id, then a unique discovered name (a name shared by several sessions is an error that lists their pane ids). Discovered sessions get the Claude envelope.

`--bypass` starts Claude with `--dangerously-skip-permissions`. In a folder Claude has not trusted yet, `spawn` stops and asks you to accept Claude's prompt in the new pane instead of answering it for you.

## Telegram

1. Create a bot with [@BotFather](https://t.me/BotFather) (`/newbot`) and copy the token.
2. Run `colony telegram connect <token>`. The bot runs in that pane until Ctrl+C (add `--new-pane` inside herdr or tmux to run it in its own `telegram-bot` pane, e.g. from a Claude session).
3. Send any message to your bot. The first person to message it after `connect` is locked in as its only user.

From your phone:

| Message | Effect |
|---|---|
| `/list` | List sessions |
| `/use <name>` / `/use off` / `/use` | Set, clear or show the default session for plain text (same setting as `colony telegram receiver`; the last one set wins) |
| `/send <name> <message>` | Send to a session; it answers straight to the chat. A busy session is told so; one waiting on a prompt is not sent to |
| `/last <name> [lines]` | Show a session's recent output |
| `/help` | List commands |
| plain text | Goes to the default session (`/use`), if one is set |

The bot registers these commands with Telegram each time it starts, so the `/` menu in the chat lists them.

Answers reach Telegram only for messages that came from Telegram.

## Files

State lives in `~/.colony/`:

- `names.json` — pane names
- `telegram.json` — bot token and lock (mode 600)
- `pending.json` — open Telegram reply ids (valid 24 hours)
- `colony.log` — one line per send, reply, spawn, close and register

## Documentation

- [Architecture](docs/architecture.md) — backends, state files, module map
- [Messaging](docs/messaging.md) — envelopes, reply IDs, spawn intro
- [Telegram bot](docs/telegram.md) — lock rules, commands, reply flow, security
- [Spawn layout](docs/layout.md) — how panes are arranged on herdr and tmux
- [Development](docs/development.md) — build, tests, CI and releases
- [History and decisions](plan/HISTORY.md) — why colony exists and why it works this way

## Release notes

See [RELEASE_NOTES.md](RELEASE_NOTES.md), including known limitations.
