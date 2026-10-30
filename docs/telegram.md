# Telegram bot

The bot lets one person list sessions, message them and read their output from a phone. It is part of the same binary: `colony telegram run`.

## Running it

`colony telegram connect <token>`:

1. Calls the Bot API `getMe` to check the token.
2. Saves the token and the connect time in `~/.colony/telegram.json` (mode 600). If the token belongs to the same bot as before, the user lock is kept; for a different bot the lock and the polling offset are reset.
3. Runs the bot loop **in the current pane**, in the foreground (same as `colony telegram run`), until Ctrl+C or until `disconnect` clears the token. It prints `Connected as @bot - bot running here, Ctrl+C to stop` first. It opens no pane and registers no name, and it works without herdr or tmux.

`colony telegram connect <token> --new-pane` is for a Claude session that must not block its own pane: it opens a pane named `telegram-bot` next to the current one and runs `colony telegram run` in it. Outside herdr and tmux it prints the command to run yourself.

The running loop records its PID and start time in `telegram.json` and clears them on a clean exit. `connect` refuses to start a second bot when a live bot process for the same token exists, and says where it runs. If the token is new while a bot is alive, that bot switches to the new token and no second one is started.

`colony telegram disconnect` clears the token, lock, receiver and all pending reply IDs; the bot loop exits when the token is gone. It closes the `telegram-bot` pane only if colony created it with `--new-pane`, never a pane it did not create. `colony telegram status` shows whether it is connected, the bot name, the locked user id, the receiver and `bot process: running (pid N[, pane P])`. The process counts as running only if its PID is alive and (where `/proc` exists) its command line is a colony process; otherwise `not running`.

## Polling

The bot uses long polling (`getUpdates`, 30 second timeout). It needs no web server and no public address. The HTTP client scrubs the token from error messages, so the token does not appear in output or logs.

On a polling error the bot waits 5 seconds and retries. The API client is rebuilt from the saved token on every pass, so a reconnect with a new token is picked up without restarting.

## Who can use it (the lock)

- Only **private chats** are handled. Group chats, messages without text and updates without a sender are ignored.
- After `connect`, the **first private message** locks the bot to its sender (user id and chat id). That message is not run as a command; the bot answers "🔒 locked to you, send /help".
- Everyone else is **ignored silently**.
- Messages dated **at or before the connect time** are ignored entirely, so a backlog Telegram replays from before `connect` cannot claim the lock or run commands. Dates have one-second resolution, so a message in the exact connect second is dropped too; send it again.
- Without a connect time nobody can claim the lock.

## Offset

The next `getUpdates` offset is saved in `telegram.json` together with the id of the bot it belongs to, and it is saved **before** an update is handled. A restart therefore does not run a `/send` a second time. Connecting a different bot resets the offset, and an offset is only written while the saved token still belongs to the bot the update came from, so one bot's offset is never applied to another. If handling or answering one update fails, it is reported on stderr and the update is still consumed.

## Commands

Telegram command names allow only `a-z 0-9 _`, so the list command is `/list`.

| Message | Action |
|---|---|
| `/list` | one line per session: name, kind, status, backend and pane; unregistered Claude sessions are included, kind `claude*`, with a `* not registered` line |
| `/use <name>` | set the default session for plain text; `<name>` resolves like `/send` (registered name, pane id or discovered name); unknown or ambiguous: error reply, nothing changed |
| `/use` | show the current default session |
| `/use off` | clear the default session |
| `/send <name> <message>` | `<name>` is a registered or discovered name or a pane id; forward with a Telegram envelope; the bot answers "📨 sent to <name>" (see Busy and blocked sessions) |
| `/last <name> [lines]` | recent output (default 50, at most 1000 lines), trimmed to the last lines that fit one message |
| `/help` (or anything unknown) | list the commands |
| plain text | forward to the default session (`/use`) |

Plain text with no default session gets "❓ no session selected, use /use <name> or /send <name> <msg>". If the default session's pane is gone, "⚠️ receiver <name> not found, use /send". A name that is not a live session gets "⚠️ no session named <name>". `/use <name>` answers "🎯 default session: <name>" and `/use off` "🎯 default session cleared".

Bot answers (`/list`, `/last`, errors) are sent as plain text (no Markdown parsing) with emoji: `/list` rows start with a status dot (🟢 idle, 🟡 working, 🔴 blocked, ⚪ unknown or tmux), errors start with ⚠️, and each `/help` line has one emoji and split at Telegram's 4096-unit limit; only the first chunk quotes the original message.

`colony telegram reply` renders simple Markdown as Telegram HTML (`parse_mode` HTML): `**bold**`/`__bold__`, `` `code` ``, fenced blocks, `- `/`* ` bullets (shown as `•`) and `#` headings; `&`, `<` and `>` are escaped. It splits on line boundaries after conversion (a fenced block spanning chunks is closed and reopened). If Telegram rejects a chunk (for example "can't parse entities"), that chunk is resent once as the original plain text. Tables are not rendered.

Every reply starts with a sender header, added by colony (sessions must not add their own). The name is the session that ran `reply`: its registered colony name, else its discovered display name, else its pane id; outside a multiplexer it is the target name of the reply ID, else `colony`. The name is upper-cased, bold and escaped. The header is only on the first message when a reply is split, and it stays in the plain-text fallback. Example (14 `━`):

```
🤖 **BO-SERVER**
━━━━━━━━━━━━━━

✅ Build passed
• 142 tests green
```

## Command menu

Every time the bot loop starts (and at `connect`, before the bot runs) it registers `list`, `use`, `send`, `last` and `help` with the Bot API `setMyCommands`, so Telegram shows them in the `/` menu. If that call fails the error is logged to stderr and the bot runs on.

## Busy and blocked sessions

When `/send` or plain text reaches a herdr session whose status is `working`, the answer is "📨 sent to <name> (⏳ busy — it will answer after its current task)". An idle session gets "📨 sent to <name>", and so does a tmux session (it has no status). A session that is `blocked` (waiting on a prompt) is not sent to: the answer is "⛔ not delivered: <name> is waiting on a prompt", and no reply ID is created.

## Receiver session

`/use <name>` and `colony telegram receiver <name>` are the same setting (`receiver` in `telegram.json`); whichever was set last wins. `colony telegram receiver <name>` picks a session that gets plain-text messages; `--clear` removes it. It is optional: the slash commands work without one, use no Claude tokens and still work if the receiver is down. A Claude session can make itself the receiver when you ask it to. The receiver sees the Telegram envelope and may forward the message to another session with `--from telegram:<id>`.

## Reply flow

1. A message arrives from the locked user (`/send` or plain text).
2. The bot creates a reply ID, stores it in `pending.json` and sends the Telegram envelope to the target. If delivery fails, the ID is removed again.
3. The target runs `colony telegram reply <id> "<answer>"`. That call reads `pending.json`, checks the ID is at most 24 hours old and sends the text to the stored chat, quoting the original message.
4. The ID stays valid for more answers until it expires.

Only messages that carry a reply ID are ever sent to Telegram, and nothing is sent to Telegram for messages that did not come from Telegram.

## Security notes

- One locked user; the lock is saved in a mode-600 file.
- `telegram.json` and `pending.json` are mode 600 (they hold the token and chat ids); `~/.colony` is mode 700.
- The bot token is visible in `ps` and shell history while `colony telegram connect` runs.
- A session that knows a valid reply ID can post to the chat; IDs are random, short-lived and only sent to the session that should answer.
- A crashed bot is shown as `not running` once its PID is gone; a stale PID that was reused by a non-colony process is ignored.
- Treat the locked chat like a remote control for your sessions: anything you can `/send` is typed into a Claude session, which may run commands.
