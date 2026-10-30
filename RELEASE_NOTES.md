# Release Notes

## colony 1.2.0 — 2026-10-30 (WIB, UTC+7)

### New

- **Auto-discovery of unregistered Claude sessions.** `colony list` and Telegram `/list` now also show Claude Code sessions that have no name in colony, marked `claude*` with a `* not registered` line. Their name is the one set with Claude's `/rename` (read from `~/.claude/sessions`), or the pane id if none is found. Nothing is written to `names.json`.
- **Target them by that name.** `send`, `read`, `wait`, `close`, `/send` and `/last` accept a discovered name; a registered name wins, then a pane id, then a unique discovered name (a name shared by several sessions is an error listing their pane ids).
- **`/use <name>`** sets the default session for plain-text phone messages; `/use` shows it and `/use off` clears it. It is the same setting as `colony telegram receiver`; the last one set wins. Plain text with no default gets "❓ no session selected, use /use <name> or /send <name> <msg>".
- **`/` command menu.** The bot registers `list`, `use`, `send`, `last` and `help` with Telegram every time it starts and at `connect`, so they show in the chat menu. A failure is logged and never stops the bot.
- **Busy and blocked notice.** A message to a `working` session is acknowledged with "📨 sent to <name> (⏳ busy — it will answer after its current task)". A session waiting on a prompt is not sent to: "⛔ not delivered: <name> is waiting on a prompt", and no reply ID is left behind.

- **Tidier phone replies.** The Telegram envelope now asks sessions for a phone-friendly format (result first, one fact per line with •, no long paragraphs). `colony telegram reply` renders simple Markdown in Telegram (`**bold**`, `` `code` ``, fenced blocks, `-`/`*` bullets, `#` headings); anything else is escaped. If Telegram rejects a message, that part is resent once as plain text, so nothing is lost.
- **Sender header on replies.** Every `colony telegram reply` starts with `🤖 **NAME**`, a `━━━━━━━━━━━━━━` line and an empty line, so the chat shows which session answered. NAME is the replying session's registered colony name, else its `/rename` name, else its pane id (outside herdr/tmux: the reply target's name, else `colony`), upper-cased and escaped. When a reply is split, only the first message has it. Sessions must not add their own header.
- **Emoji.** Bot replies carry emoji: status dots in `/list` (🟢 idle, 🟡 working, 🔴 blocked, ⚪ unknown), 📨 on acknowledgements, ⛔ not delivered, 🎯 default session, 🔒 lock, ⚠️ on errors, one per `/help` line; they stay plain text. The Telegram envelope also asks sessions for a few fitting emoji (✅ ❌ ⏳ ⚠️ 📌).

### Download

Download `colony-1.2.0-x86_64-linux.tar.gz` (static Linux x86_64 build, with a `.sha256` file), then:

```bash
tar xzf colony-1.2.0-x86_64-linux.tar.gz
./colony --help
```

## colony 1.1.0 — 2026-10-30 (WIB, UTC+7)

### Changed

- **`colony telegram connect <token>` runs the bot in the current pane** until Ctrl+C, instead of opening a new pane. Use `--new-pane` for the old behaviour (a separate `telegram-bot` pane), e.g. when a Claude session runs `connect` and must not block its own pane.
- **`colony telegram status` checks the bot process**, not just its pane: the last line shows `bot process: running (pid N)` or `not running`. A crashed bot is no longer reported as running.
- **`colony telegram disconnect`** stops the bot and closes a pane only if colony opened it with `--new-pane`.
- `connect` does not start a second bot when one is already running; a running bot picks up a new token by itself.

### New

- **Claude Code skill** in `skills/colony/SKILL.md`: teaches Claude sessions to use colony (spawning, messaging, replying to `[COLONY MSG]` blocks and Telegram). Install with `cp -r skills/colony ~/.claude/skills/`.

### Download

Download `colony-1.1.0-x86_64-linux.tar.gz` (static Linux x86_64 build, with a `.sha256` file), then:

```bash
tar xzf colony-1.1.0-x86_64-linux.tar.gz
./colony --help
```

## colony 1.0.0 — 2026-10-30 (WIB, UTC+7)

First release of colony 2: a Rust rewrite of the Bash `colony` script (kept in `old-colony/`).
One binary drives Claude Code sessions in **herdr and tmux** panes, and runs a **Telegram bot** so you can reach those sessions from your phone.

Design notes: [docs/](docs/) and [plan/HISTORY.md](plan/HISTORY.md).

### New

- **herdr support.** Works inside herdr panes as well as tmux; one process can reach sessions in both.
- **Names in a file.** Pane names live in `~/.colony/names.json` instead of tmux environment variables, the same for herdr and tmux. Dead panes are dropped on `colony list`.
- **`colony list`** shows name, pane, backend, kind, status (herdr: idle / working / blocked) and cwd.
- **`colony spawn [<cmd>] [<name>] [--cwd DIR] [--bypass] [--direction right|down]`.** `colony spawn claude bo --cwd DIR --bypass` starts and names a Claude session in one step. No command opens a shell. `colony split` is kept as an alias.
- **`colony send "<msg>" <name|id> [--wait] [--from telegram:<id>]`.** Claude panes get a `[COLONY MSG]` envelope; other panes get raw text. Text starting with `-` is accepted.
- **`colony read <name|id> [--lines N]`** shows a session's recent output.
- **`colony wait <name|id> --until idle|blocked [--timeout SEC]`** (herdr only).
- **`colony close <name|id>`** closes the pane and drops the name.
- **`colony register <name> [pane-id]`** names a pane colony did not start.
- **Telegram bot:**
  - `colony telegram connect <API_TOKEN>` checks the token, saves it (`~/.colony/telegram.json`, mode 600) and starts the bot in its own `telegram-bot` pane.
  - `colony telegram disconnect`, `status`, `receiver <name>` / `receiver --clear`.
  - The first person to message the bot after `connect` is locked in as the only user; group chats and everyone else are ignored.
  - Phone commands: `/list`, `/send <name> <msg>`, `/last <name> [lines]`, `/help`. Plain text goes to the optional receiver session.
  - Sessions answer with `colony telegram reply <id> "<text>"`, which goes straight to your phone. Reply IDs are valid for 24 hours.
  - Only messages that came from Telegram are ever answered on Telegram. No Claude hooks are used.
- **Log.** `~/.colony/colony.log` records send, reply, spawn, close and register with WIB timestamps.

### Changed from the Bash version

- No "main" pane. New spawn layout: the lead session keeps the left 50% of the window, and spawned panes stack in a right column with equal heights. `--direction right|down` overrides this.
- The spawn intro names the session that ran `spawn` as the one to report to.
- A "reply to" instruction is added only when the sender is a Claude session, so a reply is never typed into a plain shell.
- In a folder Claude has not trusted yet, `spawn` stops with a clear message instead of answering Claude's trust or bypass prompt for you.

### Security fixes made before release

- Old Telegram messages (sent before `connect`) can no longer claim the lock.
- Handled Telegram messages are not run again after a bot restart; the position is saved per bot.
- Connecting a new bot while the old one runs no longer mixes the two bots' messages.

### Known limitations

- herdr and tmux were live-tested (spawn, send/reply, read, wait, list, close, layout). The Telegram bot is not live-tested yet.
- The bot token is visible in `ps` and shell history while `colony telegram connect` runs.
- After a tmux server restart, an old name can point at an unrelated pane until `colony list` or `close` cleans it up.
- `send --wait` can return before the target starts its new turn.
- `register` uses the current environment's backend even for a pane id of the other backend.
- A crashed bot still counts as running while its pane stays open.

### Download

Download `colony-1.0.0-x86_64-linux.tar.gz` (static Linux x86_64 build, with a `.sha256` file), then:

```bash
tar xzf colony-1.0.0-x86_64-linux.tar.gz
./colony --help
```

### Build

```bash
./install-rust.sh   # Rust 1.99.1 via the flake, then cargo test (2 cores)
```
