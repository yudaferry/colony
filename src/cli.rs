//! clap definitions. Help text comes from the doc comments.

use crate::backend::{AgentState, Direction};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "colony", version, about = "Drive herdr and tmux panes; run a locked Telegram bot")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

/// Where a forwarded message came from (`--from telegram:<reply-id>`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Origin {
    Telegram(String),
}

fn parse_origin(s: &str) -> Result<Origin, String> {
    match s.strip_prefix("telegram:") {
        Some(id) if !id.is_empty() => Ok(Origin::Telegram(id.to_string())),
        _ => Err(format!("expected telegram:<reply-id>, got '{s}'")),
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List registered panes (name, pane id, backend, kind, status, cwd)
    List,
    /// Name an existing pane (default: the current pane)
    Register {
        /// Colony name for the pane
        name: String,
        /// Pane id (default: current pane)
        pane_id: Option<String>,
    },
    /// Open a new pane; with a command, run it; `claude` starts a Claude session
    Spawn {
        /// Command to run (`claude`, `opencode`, ...); none opens a shell
        cmd: Option<String>,
        /// Colony name for the new pane (required for `claude`)
        name: Option<String>,
        /// Working directory of the new pane
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Start Claude with --dangerously-skip-permissions
        #[arg(long)]
        bypass: bool,
        /// Plain split of the current pane in this direction (default: 50% right
        /// column, later panes stacked below it with equal heights)
        #[arg(long)]
        direction: Option<Direction>,
    },
    /// Open a new shell pane (alias for `spawn` with no command)
    Split,
    /// Send a message to a pane
    Send {
        /// Message text
        #[arg(allow_hyphen_values = true)]
        message: String,
        /// Target name or pane id
        target: String,
        /// Block until the target is idle again (herdr only)
        #[arg(long)]
        wait: bool,
        /// Forward a Telegram message: telegram:<reply-id>
        #[arg(long, value_parser = parse_origin)]
        from: Option<Origin>,
    },
    /// Print the recent output of a pane
    Read {
        /// Target name or pane id
        target: String,
        /// Number of lines
        #[arg(long, default_value_t = 50)]
        lines: usize,
    },
    /// Wait until an agent reaches a state (herdr only)
    Wait {
        /// Target name or pane id
        target: String,
        /// State to wait for
        #[arg(long)]
        until: AgentState,
        /// Timeout in seconds
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Close a pane and drop its names
    Close {
        /// Target name or pane id
        target: String,
    },
    /// Telegram bot commands
    Telegram {
        #[command(subcommand)]
        cmd: TgCmd,
    },
    /// Scheduled jobs (they run only while `colony telegram run` is running)
    Schedule {
        #[command(subcommand)]
        cmd: SchedCmd,
    },
}

/// Optional limits of a job. Times are "YYYY-MM-DD HH:MM" in the job's --tz (WIB by default).
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct LimitArgs {
    /// Remove the job after this many runs
    #[arg(long, value_name = "N")]
    pub times: Option<u32>,
    /// No runs before this time, "YYYY-MM-DD HH:MM". Also takes telegram:<reply-id> (the request came
    /// from the phone: finish and error notices go to that chat); repeat --from to give both
    #[arg(long, value_name = "TIME|telegram:ID")]
    pub from: Vec<String>,
    /// Remove the job once this time is passed (no run at or after it), "YYYY-MM-DD HH:MM"
    #[arg(long, value_name = "TIME")]
    pub until: Option<String>,
}

impl LimitArgs {
    /// The `--from` time (a value that is not `telegram:<id>`); at most one.
    pub fn start(&self) -> Result<Option<&str>, String> {
        let mut t = self.from.iter().map(String::as_str).filter(|v| !v.starts_with("telegram:"));
        let first = t.next();
        if t.next().is_some() {
            return Err("--from time given twice".to_string());
        }
        Ok(first)
    }

    /// The `--from telegram:<reply-id>` origin, if given (at most one).
    pub fn origin(&self) -> Result<Option<Origin>, String> {
        let mut o = self.from.iter().filter(|v| v.starts_with("telegram:"));
        let first = o.next().map(|v| parse_origin(v)).transpose()?;
        if o.next().is_some() {
            return Err("--from telegram:<reply-id> given twice".to_string());
        }
        Ok(first)
    }
}

#[derive(Subcommand, Debug)]
pub enum SchedCmd {
    /// List the jobs with their next run (WIB)
    List,
    /// Add a job that sends a message to a session, which answers on the phone
    AddMessage {
        /// Job id (letters, digits, '-' and '_')
        id: String,
        /// 5-field cron expression: minute hour day month weekday
        #[arg(long)]
        cron: String,
        /// IANA timezone of the cron expression
        #[arg(long, default_value = "Asia/Jakarta")]
        tz: String,
        #[command(flatten)]
        limits: LimitArgs,
        /// Session name or pane id that receives the message
        target: String,
        /// Message text
        #[arg(allow_hyphen_values = true)]
        message: String,
    },
    /// Add a job that runs a shell command; it alerts the phone on a non-zero exit or any output
    AddCommand {
        /// Job id (letters, digits, '-' and '_')
        id: String,
        /// 5-field cron expression: minute hour day month weekday
        #[arg(long)]
        cron: String,
        /// IANA timezone of the cron expression
        #[arg(long, default_value = "Asia/Jakarta")]
        tz: String,
        /// Also forward the output to this session so it can investigate
        #[arg(long, value_name = "SESSION")]
        notify: Option<String>,
        #[command(flatten)]
        limits: LimitArgs,
        /// Shell command (run with `sh -c` in $HOME, 120 s timeout)
        #[arg(allow_hyphen_values = true)]
        command: String,
    },
    /// Remove a job
    Rm {
        /// Job id
        id: String,
    },
    /// Pause a job: `pause <id> 30m|2h|1d` or `pause <id> until "YYYY-MM-DD HH:MM"`. Skipped runs are not replayed
    Pause {
        /// Job id
        id: String,
        /// A duration (30m, 2h, 1d) or `until "YYYY-MM-DD HH:MM"`
        #[arg(num_args = 1..=2, required = true)]
        when: Vec<String>,
        /// IANA timezone of the `until` time
        #[arg(long, default_value = "Asia/Jakarta")]
        tz: String,
    },
    /// End a pause early
    Resume {
        /// Job id
        id: String,
    },
    /// Enable a job
    Enable {
        /// Job id
        id: String,
    },
    /// Disable a job (kept in the file, never fires)
    Disable {
        /// Job id
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TgCmd {
    /// Validate and save a bot token, then run the bot here until Ctrl+C
    Connect {
        /// Bot API token
        token: String,
        /// Run the bot in a new `telegram-bot` pane instead of this one
        #[arg(long)]
        new_pane: bool,
    },
    /// Stop the bot and delete the token and lock
    Disconnect,
    /// Show connection status
    Status,
    /// Set or clear the receiver session
    Receiver {
        /// Session name that receives plain Telegram text
        #[arg(conflicts_with = "clear", required_unless_present = "clear")]
        name: Option<String>,
        /// Clear the receiver
        #[arg(long)]
        clear: bool,
    },
    /// Send an answer to the Telegram chat for a reply id
    Reply {
        /// Reply id from the message envelope
        reply_id: String,
        /// Answer text (optional with --photo / --file)
        #[arg(allow_hyphen_values = true, required_unless_present_any = ["photo", "file"])]
        text: Option<String>,
        /// Send this image (sendPhoto, up to 10 MB); the text becomes its caption
        #[arg(long, value_name = "PATH", conflicts_with = "file")]
        photo: Option<PathBuf>,
        /// Send this file as a document (sendDocument, up to 50 MB); the text becomes its caption
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// Run the bot loop
    Run,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn spawn_no_args_is_shell() {
        match parse(&["colony", "spawn"]).cmd.unwrap() {
            Cmd::Spawn { cmd, name, cwd, bypass, direction } => {
                assert_eq!(cmd, None);
                assert_eq!(name, None);
                assert_eq!(cwd, None);
                assert!(!bypass);
                assert_eq!(direction, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn spawn_cmd_then_name() {
        let c = parse(&["colony", "spawn", "claude", "bo-server", "--cwd", "/x", "--bypass"]);
        match c.cmd.unwrap() {
            Cmd::Spawn { cmd, name, cwd, bypass, .. } => {
                assert_eq!(cmd.as_deref(), Some("claude"));
                assert_eq!(name.as_deref(), Some("bo-server"));
                assert_eq!(cwd, Some(PathBuf::from("/x")));
                assert!(bypass);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn spawn_direction_down() {
        match parse(&["colony", "spawn", "--direction", "down"]).cmd.unwrap() {
            Cmd::Spawn { direction, .. } => assert_eq!(direction, Some(Direction::Down)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn split_parses() {
        assert!(matches!(parse(&["colony", "split"]).cmd, Some(Cmd::Split)));
    }

    #[test]
    fn send_message_first_then_target() {
        match parse(&["colony", "send", "hello there", "bo"]).cmd.unwrap() {
            Cmd::Send { message, target, wait, from } => {
                assert_eq!(message, "hello there");
                assert_eq!(target, "bo");
                assert!(!wait);
                assert_eq!(from, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn reply_text_may_start_with_hyphen() {
        match parse(&["colony", "telegram", "reply", "k3f9qa", "- done\n- tests pass"]).cmd.unwrap() {
            Cmd::Telegram { cmd: TgCmd::Reply { reply_id, text, .. } } => {
                assert_eq!(reply_id, "k3f9qa");
                assert_eq!(text.as_deref(), Some("- done\n- tests pass"));
            }
            c => panic!("{c:?}"),
        }
    }

    #[test]
    fn send_message_may_start_with_hyphen() {
        match parse(&["colony", "send", "-v broke", "bo"]).cmd.unwrap() {
            Cmd::Send { message, target, wait, .. } => {
                assert_eq!(message, "-v broke");
                assert_eq!(target, "bo");
                assert!(!wait);
            }
            c => panic!("{c:?}"),
        }
    }

    #[test]
    fn send_wait_after_positionals() {
        match parse(&["colony", "send", "x", "bo", "--wait"]).cmd.unwrap() {
            Cmd::Send { message, target, wait, .. } => {
                assert_eq!((message.as_str(), target.as_str()), ("x", "bo"));
                assert!(wait);
            }
            c => panic!("{c:?}"),
        }
    }

    #[test]
    fn send_from_telegram_id() {
        match parse(&["colony", "send", "hi", "bo", "--from", "telegram:k3f9qa"])
            .cmd
            .unwrap()
        {
            Cmd::Send { from, .. } => {
                assert_eq!(from, Some(Origin::Telegram("k3f9qa".to_string())))
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn send_from_bad_prefix_errors() {
        assert!(Cli::try_parse_from(["colony", "send", "hi", "bo", "--from", "slack:x"]).is_err());
        assert!(Cli::try_parse_from(["colony", "send", "hi", "bo", "--from", "telegram:"]).is_err());
    }

    #[test]
    fn read_lines_default_50() {
        match parse(&["colony", "read", "bo"]).cmd.unwrap() {
            Cmd::Read { target, lines } => {
                assert_eq!(target, "bo");
                assert_eq!(lines, 50);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn wait_until_blocked() {
        match parse(&["colony", "wait", "bo", "--until", "blocked"]).cmd.unwrap() {
            Cmd::Wait { until, timeout, .. } => {
                assert_eq!(until, AgentState::Blocked);
                assert_eq!(timeout, 600);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn receiver_name_and_clear_conflict_errors() {
        assert!(Cli::try_parse_from(["colony", "telegram", "receiver", "bo", "--clear"]).is_err());
        assert!(Cli::try_parse_from(["colony", "telegram", "receiver"]).is_err());
        assert!(Cli::try_parse_from(["colony", "telegram", "receiver", "bo"]).is_ok());
        assert!(Cli::try_parse_from(["colony", "telegram", "receiver", "--clear"]).is_ok());
    }

    #[test]
    fn telegram_connect_new_pane_flag() {
        match parse(&["colony", "telegram", "connect", "1:x"]).cmd.unwrap() {
            Cmd::Telegram { cmd: TgCmd::Connect { new_pane, .. } } => assert!(!new_pane),
            other => panic!("{other:?}"),
        }
        match parse(&["colony", "telegram", "connect", "1:x", "--new-pane"]).cmd.unwrap() {
            Cmd::Telegram { cmd: TgCmd::Connect { new_pane, .. } } => assert!(new_pane),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn telegram_reply_args() {
        match parse(&["colony", "telegram", "reply", "k3f9qa", "done"]).cmd.unwrap() {
            Cmd::Telegram { cmd: TgCmd::Reply { reply_id, text, photo, file } } => {
                assert_eq!(reply_id, "k3f9qa");
                assert_eq!(text.as_deref(), Some("done"));
                assert_eq!((photo, file), (None, None));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn telegram_reply_photo_and_file_args() {
        match parse(&["colony", "telegram", "reply", "k3f9qa", "--photo", "/tmp/a.png"]).cmd.unwrap() {
            Cmd::Telegram { cmd: TgCmd::Reply { text, photo, file, .. } } => {
                assert_eq!(text, None);
                assert_eq!(photo, Some(PathBuf::from("/tmp/a.png")));
                assert_eq!(file, None);
            }
            other => panic!("unexpected {other:?}"),
        }
        match parse(&["colony", "telegram", "reply", "k3f9qa", "cap", "--file", "r.pdf"]).cmd.unwrap() {
            Cmd::Telegram { cmd: TgCmd::Reply { text, file, .. } } => {
                assert_eq!(text.as_deref(), Some("cap"));
                assert_eq!(file, Some(PathBuf::from("r.pdf")));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(Cli::try_parse_from(["colony", "telegram", "reply", "k3f9qa"]).is_err());
        assert!(Cli::try_parse_from(["colony", "telegram", "reply", "k", "--photo", "a", "--file", "b"]).is_err());
    }

    #[test]
    fn schedule_args() {
        match parse(&["colony", "schedule", "add-message", "m", "--cron", "0 9 * * *", "bo", "-hi"]).cmd.unwrap() {
            Cmd::Schedule { cmd: SchedCmd::AddMessage { id, cron, tz, target, message, .. } } => {
                assert_eq!((id.as_str(), cron.as_str(), tz.as_str()), ("m", "0 9 * * *", "Asia/Jakarta"));
                assert_eq!((target.as_str(), message.as_str()), ("bo", "-hi"));
            }
            other => panic!("{other:?}"),
        }
        match parse(&["colony", "schedule", "add-command", "d", "--cron", "* * * * *", "--tz", "UTC", "--notify", "bo", "df -h"])
            .cmd
            .unwrap()
        {
            Cmd::Schedule { cmd: SchedCmd::AddCommand { tz, notify, command, .. } } => {
                assert_eq!((tz.as_str(), notify.as_deref(), command.as_str()), ("UTC", Some("bo"), "df -h"));
            }
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["colony", "schedule", "add-command", "d", "df"]).is_err(), "--cron is required");
        assert!(matches!(parse(&["colony", "schedule", "rm", "x"]).cmd, Some(Cmd::Schedule { cmd: SchedCmd::Rm { .. } })));
    }

    #[test]
    fn schedule_limit_and_pause_args() {
        match parse(&[
            "colony", "schedule", "add-command", "d", "--cron", "* * * * *", "--times", "3", "--from", "2026-10-30 09:00",
            "--until", "2026-10-30 18:00", "df",
        ])
        .cmd
        .unwrap()
        {
            Cmd::Schedule { cmd: SchedCmd::AddCommand { limits, .. } } => {
                assert_eq!(limits.times, Some(3));
                assert_eq!(limits.start().unwrap(), Some("2026-10-30 09:00"));
                assert_eq!(limits.until.as_deref(), Some("2026-10-30 18:00"));
            }
            other => panic!("{other:?}"),
        }
        match parse(&["colony", "schedule", "add-message", "m", "--cron", "* * * * *", "--times", "2", "bo", "hi"]).cmd.unwrap() {
            Cmd::Schedule { cmd: SchedCmd::AddMessage { limits, .. } } => assert_eq!(limits.times, Some(2)),
            other => panic!("{other:?}"),
        }
        match parse(&["colony", "schedule", "pause", "x", "until", "2026-10-30 18:00"]).cmd.unwrap() {
            Cmd::Schedule { cmd: SchedCmd::Pause { id, when, tz } } => {
                assert_eq!((id.as_str(), when.len(), tz.as_str()), ("x", 2, "Asia/Jakarta"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(parse(&["colony", "schedule", "pause", "x", "2h"]).cmd, Some(Cmd::Schedule { cmd: SchedCmd::Pause { .. } })));
        assert!(matches!(parse(&["colony", "schedule", "resume", "x"]).cmd, Some(Cmd::Schedule { cmd: SchedCmd::Resume { .. } })));
        assert!(Cli::try_parse_from(["colony", "schedule", "pause", "x"]).is_err());
        assert!(Cli::try_parse_from(["colony", "schedule", "add-command", "d", "--cron", "* * * * *", "--times", "x", "df"]).is_err());
    }

    #[test]
    fn no_args_gives_no_command() {
        assert!(parse(&["colony"]).cmd.is_none());
    }
}
