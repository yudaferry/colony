# History and decisions

A short account of why colony exists and why it looks the way it does. For how it works today, see [docs/](../docs/).

## The problem

Working on several projects at once means several Claude Code sessions, one per project, each in its own terminal pane (herdr or tmux). Two things get awkward:

1. **At the desk**, you want one "lead" session to start, name and message the others, and let them talk to each other, instead of you switching panes by hand.
2. **Away from the desk**, you want to ask a session how it is doing, or give it a short instruction, from a phone, without running a heavy agent platform.

colony is the one small engine behind both: a command-line tool that you, Claude sessions and a Telegram bot all call.

## Goals

- Work in both herdr and tmux, and let one process reach sessions in either.
- Spawn, name, message, read, wait for and close sessions with a few commands that Claude can run for you from plain-language requests.
- Let sessions message each other with an envelope that says who wrote it and how to reply.
- A Telegram bot with `/list`, `/send`, `/last`, plus plain text to an optional receiver session. Answers go straight back to the chat.

## Non-goals

- Memory across sessions, scheduling, multi-platform messaging or self-made skills. Those belong to full agent platforms.
- Desktop notifications (possible later).
- Managing how colony is installed on a particular machine.

## Anti-goals

Things that count as a failure even if everything else works:

- **Adding any Claude Code hook.** Sessions reply by running a colony command.
- **Sending anything to Telegram that did not come from Telegram.**
- **Breaking existing tmux use.**

## Key decisions

| Decision | Reason |
|---|---|
| Rewrite the earlier tmux-only shell script in Rust | The script could not work inside herdr and was awkward to extend. A long-running bot with HTTP and JSON is painful in shell. One static binary serves the CLI and the bot, with good libraries for both. |
| Support herdr and tmux behind one `Backend` trait | People use both. Named targets keep the backend they were created on, so a bot in one multiplexer can reach a session in the other. |
| Store names in `~/.colony/names.json` | Works the same for both multiplexers; tmux environment variables do not exist in herdr. |
| No Claude hooks | Hooks need per-machine setup and are easy to break. A command that the envelope asks the session to run is simpler and visible in the transcript. |
| Telegram replies only for Telegram-origin messages | Keeps the phone quiet and prevents a session from posting to the chat on its own. A reply is possible only with a reply ID that the bot issued. |
| Direct replies with `colony telegram reply <id>` | The answering session talks to Telegram itself. The answer never passes through the receiver session, which saves its context and avoids a relay that could fail. |
| Slash commands handled by the bot, plain text by an optional receiver | `/list`, `/send` and `/last` need no Claude session, cost no tokens and work when the receiver is down. Plain text needs interpretation, so a Claude session can handle it if you set one. |
| One locked user | The first private message after `connect` locks the bot to its sender; everyone else and all groups are ignored. Messages older than the connect are ignored so a replayed backlog cannot claim the lock. |
| Long polling | No web server or public URL is needed. |
| Reply IDs last 24 hours and are reusable | A session can send progress and then a final answer. Expiry limits how long an old ID can be used. |
| Reply instruction only for Claude senders | A reply addressed to a shell would be typed into it and run. |
| Layout: lead keeps the left 50%, spawned panes stack on the right with equal heights | Predictable and tidy no matter how many sessions are spawned. `--direction` gives a plain split when you want one. |
| Spawn stops at Claude's folder-trust prompt | colony does not answer trust or permission prompts for you; you accept them in the pane. |
| Code is the source of truth | Where an early design note and the code disagree, the code wins and the docs follow it. |

## Alternatives considered

- **Claude's built-in cross-session messaging** works between named sessions but is not available from a phone and does not carry the origin of a message.
- **The official Telegram channel plugin** ties a bot to one live session and fills that session's context, so reaching other sessions depends on it staying alive.
- **Existing Telegram-to-Claude bridges** found at the time worked with tmux only.
