# Messaging

colony sends text into a pane and, for Claude sessions, wraps it in an envelope so the receiver knows who wrote it and how to answer. The exact text comes from `src/envelope.rs`.

## Who gets an envelope

- **Claude panes** get an envelope.
- **Everything else** (shells, other programs) gets the raw text followed by Enter, with no envelope. On herdr this is `pane run`; on tmux it is `send-keys`.

colony decides by the kind stored in `names.json`. An unnamed raw pane id is checked: if the pane looks like a Claude session, it is treated as one. A Claude session found by auto-discovery (not registered; see [architecture](architecture.md)) can be addressed by its session name and always gets the envelope.

## Laptop envelope

Sent by `colony send "<msg>" <target>` when the message comes from a pane, not from Telegram:

```
[COLONY MSG]
From: <sender name>
Message: <text>
Instruction: reply only if useful, with: colony send "<reply>" <sender name>
[/COLONY MSG]
```

The sender is the caller's pane, looked up by name in `names.json`. Variants:

- **Sender is a named Claude session:** the full envelope above, with the reply instruction.
- **Sender is a named non-Claude pane (a shell, say):** `From: <name>` but no `Instruction` line. A reply typed into a shell would be executed as a command, so colony never asks for one.
- **Sender has no colony name:** `From: user` and no instruction.

```
[COLONY MSG]
From: user
Message: <text>
[/COLONY MSG]
```

## Telegram envelope

Sent when the origin is Telegram (`colony send ... --from telegram:<id>`, used by the bot):

```
[COLONY MSG]
From: telegram (reply-id <id>)
Message: <text>
Instruction: This came from the user's phone. Keep the answer short. Send your answer with: colony telegram reply <id> "<answer>"
[/COLONY MSG]
```

The receiving session answers with `colony telegram reply <id> "<answer>"`, which goes straight to the Telegram chat ([telegram](telegram.md)); simple Markdown in the answer is rendered, with a plain-text fallback. Answering in the terminal does not reach the phone.

## Reply IDs

- A reply ID is 6 characters from `[a-z0-9]`, so it is safe to type in a shell.
- The bot creates one for every message it forwards (plain text to the default session) and stores it in `pending.json` with the target, chat id and message id.
- An ID is valid for **24 hours** and can be **used more than once**, so a session can send progress and then a final answer.
- Expired entries are pruned whenever a new ID is created.
- `colony telegram reply` accepts any caller that knows a valid ID; it does not check who calls it. An unknown or expired ID is an error and nothing is sent.
- The receiver session forwards a message with `--from telegram:<id>`, so the final target answers Telegram directly with the same ID.

## Spawn intro

`colony spawn claude <name>` starts Claude in the new pane and sends it one first message (the intro). It tells the session:

- its name and pane id, and which session is the orchestrator (the one that ran `spawn`);
- that messages arrive between `[COLONY MSG]` and `[/COLONY MSG]` in two kinds, from another session or from Telegram;
- for the first kind, how to reply: `colony send "<reply>" <orchestrator>`;
- for the second kind, to keep the answer short and send it with `colony telegram reply <id> "<answer>"`, and not to answer in the terminal.

The reply instruction for the orchestrator is included only if the spawner is a Claude session: by name when it is registered as Claude, by raw pane id when it is unnamed but detected as Claude. If the spawner is a shell, the intro leaves that sentence out.

## No hooks

colony adds no Claude Code hook. Sessions reply because the envelope tells them to run a colony command.
