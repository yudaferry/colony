# Telegram bot

The bot lets one person list sessions, message them and read their output from a phone. It is part of the same binary: `colony telegram run`.

## Running it

`colony telegram connect <token>`:

1. Calls the Bot API `getMe` to check the token.
2. Saves the token and the connect time in `~/.colony/telegram.json` (mode 600). If the token belongs to the same bot as before, the user lock is kept; for a different bot the lock and the polling offset are reset.
3. Opens a pane named `telegram-bot` next to the current one and runs `colony telegram run` in it. If the bot pane is already alive, it does nothing more. Outside herdr and tmux, it prints the command to run yourself.

`colony telegram disconnect` closes the bot pane and clears the token, lock, receiver and all pending reply IDs. The bot loop exits when the token is gone. `colony telegram status` shows whether it is connected, the bot name, the locked user id, the receiver and whether the bot pane is alive.

## Polling

The bot uses long polling (`getUpdates`, 30 second timeout). It needs no web server and no public address. The HTTP client scrubs the token from error messages, so the token does not appear in output or logs.

On a polling error the bot waits 5 seconds and retries. The API client is rebuilt from the saved token on every pass, so a reconnect with a new token is picked up without restarting.

## Who can use it (the lock)

- Only **private chats** are handled. Group chats, messages without text and updates without a sender are ignored.
- After `connect`, the **first private message** locks the bot to its sender (user id and chat id). That message is not run as a command; the bot answers "locked to you, send /help".
- Everyone else is **ignored silently**.
- Messages dated **at or before the connect time** are ignored entirely, so a backlog Telegram replays from before `connect` cannot claim the lock or run commands. Dates have one-second resolution, so a message in the exact connect second is dropped too; send it again.
- Without a connect time nobody can claim the lock.

## Offset

The next `getUpdates` offset is saved in `telegram.json` together with the id of the bot it belongs to, and it is saved **before** an update is handled. A restart therefore does not run a `/send` a second time. Connecting a different bot resets the offset, and an offset is only written while the saved token still belongs to the bot the update came from, so one bot's offset is never applied to another. If handling or answering one update fails, it is reported on stderr and the update is still consumed.

## Commands

Telegram command names allow only `a-z 0-9 _`, so the list command is `/list`.

| Message | Action |
|---|---|
| `/list` | one line per session: name, kind, status, backend and pane |
| `/send <name> <message>` | forward with a Telegram envelope; the bot answers "sent to <name>" |
| `/last <name> [lines]` | recent output (default 50, at most 1000 lines), trimmed to the last lines that fit one message |
| `/help` (or anything unknown) | list the commands |
| plain text | forward to the receiver session |

Plain text with no receiver set gets "no receiver set, use /send". If the receiver's pane is gone, "receiver <name> not found, use /send". A name that is not a live session gets "no session named <name>".

Answers are sent as plain text (no Markdown parsing) and split at Telegram's 4096-unit limit; only the first chunk quotes the original message.

## Receiver session

`colony telegram receiver <name>` picks a session that gets plain-text messages; `--clear` removes it. It is optional: the slash commands work without one, use no Claude tokens and still work if the receiver is down. A Claude session can make itself the receiver when you ask it to. The receiver sees the Telegram envelope and may forward the message to another session with `--from telegram:<id>`.

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
- A crashed bot still counts as running while its pane stays open.
- Treat the locked chat like a remote control for your sessions: anything you can `/send` is typed into a Claude session, which may run commands.
