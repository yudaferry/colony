# Telegram bot

The bot lets one person list sessions, pick a default session and talk to it from a phone. It is part of the same binary: `colony telegram run`.

## Running it

`colony telegram connect <token>`:

1. Calls the Bot API `getMe` to check the token.
2. Saves the token and the connect time in `~/.colony/telegram.json` (mode 600). If the token belongs to the same bot as before, the user lock is kept; for a different bot the lock and the polling offset are reset.
3. Runs the bot loop **in the current pane**, in the foreground (same as `colony telegram run`), until Ctrl+C or until `disconnect` clears the token. It prints `Connected as @bot - bot running here, Ctrl+C to stop` first. It opens no pane and registers no name, and it works without herdr or tmux.

`colony telegram connect <token> --new-pane` is for a Claude session that must not block its own pane: it opens a pane named `telegram-bot` next to the current one and runs `colony telegram run` in it. Outside herdr and tmux it prints the command to run yourself.

The running loop records its PID and start time in `telegram.json` and clears them on a clean exit. `connect` refuses to start a second bot when a live bot process for the same token exists, and says where it runs. If the token is new while a bot is alive, that bot switches to the new token and no second one is started.

`colony telegram disconnect` clears the token, lock, receiver, all pending reply IDs and the recorded sent messages; the bot loop exits when the token is gone. It closes the `telegram-bot` pane only if colony created it with `--new-pane`, never a pane it did not create. `colony telegram status` shows whether it is connected, the bot name, the locked user id, the receiver and `bot process: running (pid N[, pane P])`. The process counts as running only if its PID is alive and (where `/proc` exists) its command line is a colony process; otherwise `not running`.

## Polling

The bot uses long polling (`getUpdates`, 30 second timeout). It needs no web server and no public address. The HTTP client scrubs the token from error messages, so the token does not appear in output or logs.

On a polling error the bot waits 5 seconds and retries. The API client is rebuilt from the saved token on every pass, so a reconnect with a new token is picked up without restarting.

## Who can use it (the lock)

- Only **private chats** are handled. Group chats, messages without text and updates without a sender are ignored.
- After `connect`, the **first private message** locks the bot to its sender (user id and chat id). That message is not run as a command; the bot answers "🔒 locked to you, send /list".
- Everyone else is **ignored silently**.
- Messages dated **at or before the connect time** are ignored entirely, so a backlog Telegram replays from before `connect` cannot claim the lock or run commands. Dates have one-second resolution, so a message in the exact connect second is dropped too; send it again.
- Without a connect time nobody can claim the lock.

## Offset

The next `getUpdates` offset is saved in `telegram.json` together with the id of the bot it belongs to, and it is saved **before** an update is handled. A restart therefore does not deliver a message a second time. Connecting a different bot resets the offset, and an offset is only written while the saved token still belongs to the bot the update came from, so one bot's offset is never applied to another. If handling or answering one update fails, it is reported on stderr and the update is still consumed.

## Commands

The bot has exactly two commands, `/list` and `/use`. Telegram command names allow only `a-z 0-9 _`, so the list command is `/list`.

| Message | Action |
|---|---|
| `/list` | one line per session: name, kind, status, backend and pane; unregistered Claude sessions are included, kind `claude*`, with a `* not registered` line |
| `/use <name>` | set the default session for plain text; `<name>` is a registered name, pane id or discovered name; unknown or ambiguous: error reply, nothing changed |
| `/use` | reply "🎯 Choose default session:" with inline buttons (see Inline buttons) |
| `/use off` | clear the default session |
| any other `/word` | "ℹ️ only /list and /use — type normally to talk to your session" |
| photo, document, video, audio | downloaded to `~/.colony/inbox/` and routed like text (see Photos and files from the phone) |
| plain text | forward to the default session (`/use`) |
| swipe-reply to a session's answer | forward to the session that sent that answer, default unchanged (see Reply-to routing) |

There is no `/send`, `/last`, `/help` or `/schedule`. To reach a session, pick it with `/use` and type normally; to schedule something, say so in plain text and the session runs `colony schedule ...` itself (see [scheduler](scheduler.md)).

Plain text with no default session gets "❓ no session selected, use /use <name>". If the default session's pane is gone, "⚠️ receiver <name> not found, use /use". `/use <name>` answers "🎯 default session: <name>" and `/use off` "🎯 default session cleared".

Bot answers (`/list`, errors) are sent as plain text (no Markdown parsing) with emoji: `/list` rows start with a status dot (🟢 idle, 🟡 working, 🔴 blocked, ⚪ unknown or tmux) and errors start with ⚠️. Long answers are split at Telegram's 4096-unit limit; only the first chunk quotes the original message.

`colony telegram reply` renders simple Markdown as Telegram HTML (`parse_mode` HTML): `**bold**`/`__bold__`, `` `code` ``, fenced blocks, `- `/`* ` bullets (shown as `•`) and `#` headings; `&`, `<` and `>` are escaped. It splits on line boundaries after conversion (a fenced block spanning chunks is closed and reopened). If Telegram rejects a chunk (for example "can't parse entities"), that chunk is resent once as the original plain text. Tables are not rendered.

### Images and files

`colony telegram reply <id> [text] --photo <path>` sends an image with `sendPhoto` (multipart/form-data, up to 10 MB); `--file <path>` sends any file with `sendDocument` (up to 50 MB). The two flags cannot be combined, and the text is optional when one is given. The file goes to the reply's chat, quoting the original message, with the same `🤖 NAME` header and Markdown rendering as a caption. Telegram limits captions to 1024 characters: a longer text is sent as normal messages after the file, which then carries the header only. If Telegram rejects the HTML caption, it is resent once as plain text. The file is checked before any network call: it must exist, be a regular file (never a directory), be non-empty and fit the size limit, otherwise the command fails with a clear error. The multipart body is built by hand (content type guessed from jpg/jpeg/png/gif/webp, else `application/octet-stream`); the token is never printed.

Every reply starts with a sender header, added by colony (sessions must not add their own). The name is the session that ran `reply`: its registered colony name, else its discovered display name, else its pane id; outside a multiplexer it is the target name of the reply ID, else `colony`. The name is upper-cased, bold and escaped. The header is only on the first message when a reply is split, and it stays in the plain-text fallback. Example (14 `━`):

```
🤖 **BO-SERVER**
━━━━━━━━━━━━━━

✅ Build passed
• 142 tests green
```

## Photos and files from the phone

A message with a photo (the largest size), document, video, audio or voice note is downloaded with `getFile` and the file download URL (which contains the token and is never logged) to `~/.colony/inbox/` (directory mode 700, files mode 600). The name is `<YYYY-MM-DD_HHMMSS>_<shortid>.<ext>` in WIB; the extension comes from the Telegram file path, else the mime type (photo: `jpg`, otherwise `bin`). The Bot API lets bots download up to 20 MB: a bigger file gets "⚠️ file too big (max 20 MB)" and nothing is saved.

The file is then routed exactly like text: the session of a swipe-replied answer first, else the default session. The Telegram envelope carries

```
Message: <caption, or "(no caption)">
Attachment: /home/you/.colony/inbox/2026-10-29_215203_k3f9qa.jpg
```

so the Claude session can open the path with its file-read tool. The ack is "📎 sent to <name>", with the same busy/blocked rules as text. With no session selected the answer is "❓ no session selected, ..." and the downloaded file is deleted again. Same lock rules as text (locked user, private chat only).

Files in the inbox older than 7 days are deleted when the bot starts and then hourly (a built-in job). Only regular files directly inside `~/.colony/inbox` are removed; symlinks are never followed.

## Inline buttons

`/use` with no argument sends "🎯 Choose default session:" with one inline button per session (registered and discovered, as in `/list`). A button is the status dot plus the name (🟢 idle, 🟡 working, 🔴 blocked, ⚪ unknown); the current default has "✅ " in front. There is no off button; `/use off` typed by hand clears the default, and `/use <name>` keeps working. A tap on an old `use:off` button still clears the default.

Tapping a button sends a `callback_query` (the bot asks `getUpdates` for `allowed_updates` `["message","callback_query"]`). It follows the lock rules of messages: only the locked user, only in the private chat, everyone else ignored silently, and a tap never claims the lock. The bot sets or clears the default exactly like `/use <name>` / `/use off`, answers the tap with a short toast ("🎯 bo-server-84", "🎯 default cleared") and edits the tapped message to "🎯 Default session: <name>" (or "🎯 Default session cleared") with **no keyboard**, so the buttons disappear. `callback_data` is `use:<pane id>` (or `use:#<index>` if the pane id would exceed Telegram's 64 bytes) and is resolved again against the live sessions on a tap; if the session is gone the toast is "⚠️ session gone" and the buttons stay, refreshed.

## Pinned session bar

The bot keeps one pinned message in the locked user's private chat: "📌 CURRENT SESSION: <NAME UPPERCASE>", or "📌 CURRENT SESSION: none". Its message id is stored as `pinned_message_id` in `telegram.json` (mode 600, atomic locked write).

Telegram's pinned bar at the top of the app does not follow edits of the pinned message, so the bar is replaced rather than edited. Whenever the default changes (button tap, `/use <name>`, `/use off`, or `colony telegram receiver <name>` / `--clear`) and the new text differs from the stored one (`pinned_text` in `telegram.json`), the bot unpins and deletes the old message (errors such as "message not found" or "message can't be deleted" are ignored), sends a new one, pins it (`pinChatMessage` with `disable_notification`) and stores its id and text. If the text is unchanged nothing is called. The CLI calls the Bot API itself with the saved token; when not connected or not locked it skips the bar. The bar is also refreshed when the bot loop starts, if a default is set and the user is locked.

A pin, unpin, delete or send failure is reported on stderr (the bot pane) and never undoes the default change.

## Reply-to routing

Every message `colony telegram reply` sends (all chunks, and the photo or file message) is recorded in `~/.colony/sent.json` (mode 600, atomic locked writes) as `<chat id>:<message id>` -> the sender's target (registered name, unique discovered name, else pane id) and time. Entries expire after 7 days and are pruned on every write; `disconnect` clears the file.

When the locked user swipe-replies in Telegram to such a message with plain text, the bot sends it to that session: new reply ID, Telegram envelope, busy/blocked notice. The default session is **not** changed. A reply to a message that is not recorded (the bot's own acks, `/list`, an expired entry) is routed normally, to the default session. If the recorded session is gone the answer is "⚠️ no session named <name>" and nothing is sent. A reply that starts with `/` is a command as usual (`/list`, `/use`, else the hint).

## Command menu

Every time the bot loop starts (and at `connect`, before the bot runs) it registers only `list` and `use` with the Bot API `setMyCommands`, so Telegram shows just those in the `/` menu. If that call fails the error is logged to stderr and the bot runs on.

## Busy and blocked sessions

When plain text reaches a herdr session whose status is `working`, the answer is "📨 sent to <name> (⏳ busy — it will answer after its current task)". An idle session gets "📨 sent to <name>", and so does a tmux session (it has no status). A session that is `blocked` (waiting on a prompt) is not sent to: the answer is "⛔ not delivered: <name> is waiting on a prompt", and no reply ID is created.

## Receiver session

`/use <name>` and `colony telegram receiver <name>` are the same setting (`receiver` in `telegram.json`); whichever was set last wins. `colony telegram receiver <name>` picks a session that gets plain-text messages; `--clear` removes it. It is optional: `/list` and `/use` work without one, use no Claude tokens and still work if the receiver is down. A Claude session can make itself the receiver when you ask it to. The receiver sees the Telegram envelope and may forward the message to another session with `--from telegram:<id>`.

## Reply flow

1. A text message arrives from the locked user.
2. The bot creates a reply ID, stores it in `pending.json` and sends the Telegram envelope to the target. If delivery fails, the ID is removed again.
3. The target runs `colony telegram reply <id> "<answer>"`. That call reads `pending.json`, checks the ID is at most 24 hours old and sends the text to the stored chat, quoting the original message.
4. The ID stays valid for more answers until it expires.

Only messages that carry a reply ID are ever sent to Telegram, and nothing is sent to Telegram for messages that did not come from Telegram.

## Security notes

- One locked user; the lock is saved in a mode-600 file.
- `telegram.json` and `pending.json` and `sent.json` are mode 600 (they hold the token, chat ids and message ids); `~/.colony` is mode 700.
- The bot token is visible in `ps` and shell history while `colony telegram connect` runs.
- A session that knows a valid reply ID can post to the chat; IDs are random, short-lived and only sent to the session that should answer.
- A crashed bot is shown as `not running` once its PID is gone; a stale PID that was reused by a non-colony process is ignored.
- Treat the locked chat like a remote control for your sessions: anything you type is sent into a Claude session, which may run commands.
