//! clap definitions. Help text comes from the doc comments.

use crate::backend::{AgentState, Direction};
use clap::{Parser, Subcommand};
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
}

#[derive(Subcommand, Debug)]
pub enum TgCmd {
    /// Validate and save a bot token, then start the bot pane
    Connect {
        /// Bot API token
        token: String,
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
        /// Answer text
        #[arg(allow_hyphen_values = true)]
        text: String,
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
            Cmd::Telegram { cmd: TgCmd::Reply { reply_id, text } } => {
                assert_eq!(reply_id, "k3f9qa");
                assert_eq!(text, "- done\n- tests pass");
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
    fn telegram_reply_args() {
        match parse(&["colony", "telegram", "reply", "k3f9qa", "done"]).cmd.unwrap() {
            Cmd::Telegram { cmd: TgCmd::Reply { reply_id, text } } => {
                assert_eq!(reply_id, "k3f9qa");
                assert_eq!(text, "done");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn no_args_gives_no_command() {
        assert!(parse(&["colony"]).cmd.is_none());
    }
}
