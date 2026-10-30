# Release Notes

## colony 1.4.0 — 2026-10-30 (WIB, UTC+7)

### New

- **Scheduler** (runs inside `colony telegram run`; nothing runs while the bot is stopped). Jobs live in `~/.colony/schedule.json`, cron times are 5-field (minimum 1 minute), default timezone Asia/Jakarta, missed runs are not replayed.
  - **Message jobs** send a task to a session at set times; the session answers on the phone.
  - **Command jobs** run a shell command (120 s timeout) and alert only on output or a non-zero exit — no Claude tokens when all is quiet. `--notify <session>` also forwards the output to a session to investigate.
  - **Limits and pause:** `--times N`, `--until "<time>"`, `--from "<time>"`, `colony schedule pause <id> <30m|2h|1d>` or `pause <id> until "<time>"`, `resume`, `enable`, `disable`, `rm`, `list`.
  - **Origin tracking:** each job records who created it (the session, and `--from telegram:<reply-id>` for phone requests). "🏁 finished" and "⚠️ job error" notices go back to that origin (phone or session).
  - Uses the `croner` crate.
- **Receive images and files from the phone.** Photos and documents (up to 20 MB) are saved to `~/.colony/inbox/` (private) and forwarded to the current or swipe-replied session with an `Attachment:` path, so Claude can open them. Files older than 7 days are deleted automatically.
- **Send images and files to the phone:** `colony telegram reply <id> "<caption>" --photo <path>` (up to 10 MB) or `--file <path>` (up to 50 MB).
- **Pinned "📌 CURRENT SESSION" bar** at the top of the chat, replaced whenever the default session changes (buttons, `/use`, or `colony telegram receiver`).

### Changed

- Telegram commands trimmed to **`/list` and `/use`**; scheduling and everything else is done by typing normally to a session. Other `/…` messages get a short hint.
- `/use` buttons disappear after a tap and the message shows the chosen session; the "🚫 off" button is removed (typing `/use off` still works).

### Download

Download `colony-1.4.0-x86_64-linux.tar.gz` (static Linux x86_64 build, with a `.sha256` file), then:

```bash
tar xzf colony-1.4.0-x86_64-linux.tar.gz
./colony --help
```

## colony 1.3.0 — 2026-10-30 (WIB, UTC+7)

### New

- **`/use` buttons.** `/use` with no argument now replies "🎯 Choose default session:" with one inline button per session (🟢 idle, 🟡 working, 🔴 blocked, ⚪ unknown, then the name), a "✅ " on the current default and a "🚫 off" button. Tap one to set (or clear) the default; the bot answers with a short toast and moves the ✅. Only the locked user in the private chat can tap; a session that vanished gives "⚠️ session gone" and refreshed buttons. `/use <name>` and `/use off` still work. The bot now asks Telegram for `callback_query` updates too.
- **Swipe-reply routing.** Swipe-reply in Telegram to a session's answer (the `🤖 NAME` message) and your text goes to that same session, like `/send <name> <text>`, without changing the default. `colony telegram reply` records the id of every message it sends in `~/.colony/sent.json` (mode 600, kept 7 days). A reply to anything else (acks, `/list`, old messages) goes to the default session as before.

### Changed

- `/use` with no argument no longer prints the current default as text; the ✅ in the buttons shows it.
- `colony telegram disconnect` also clears `sent.json`.

### Download

Download `colony-1.3.0-x86_64-linux.tar.gz` (static Linux x86_64 build, with a `.sha256` file), then:

```bash
tar xzf colony-1.3.0-x86_64-linux.tar.gz
./colony --help
```

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
