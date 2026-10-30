use anyhow::{Result, bail};
use clap::{CommandFactory, Parser};
use colony::backend::RealBackends;
use colony::cli::{Cli, Cmd, SchedCmd, TgCmd};
use colony::commands::{close, list, read, register, schedule, send, spawn, wait};
use colony::ctx::{Ctx, ProcessEnv, SystemClock};
use colony::state::store::StateDir;
use colony::state::telegram::{TELEGRAM_FILE, TelegramConfig};
use colony::telegram::api::{FailingTelegram, FileKind, HttpTelegram, TelegramApi};
use colony::telegram::{bot, cmds};
use std::time::Duration;

fn main() {
    match run(Cli::parse()) {
        Ok(out) => {
            if !out.is_empty() {
                print!("{out}");
                if !out.ends_with('\n') {
                    println!();
                }
            }
        }
        Err(e) => {
            eprintln!("colony: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> Result<String> {
    let Some(cmd) = cli.cmd else {
        // D-16: no arguments prints usage, exit 0.
        Cli::command().print_help()?;
        println!();
        return Ok(String::new());
    };
    let env = ProcessEnv;
    let clock = SystemClock;
    let state = StateDir::home()?;
    let backends = RealBackends::from_env(&env);
    let ctx = Ctx {
        state: &state,
        backends: &backends,
        env: &env,
        clock: &clock,
    };
    match cmd {
        Cmd::List => list::list(&ctx),
        Cmd::Register { name, pane_id } => register::register(&ctx, &name, pane_id.as_deref()),
        Cmd::Spawn {
            cmd,
            name,
            cwd,
            bypass,
            direction,
        } => spawn::spawn(
            &ctx,
            &spawn::SpawnArgs {
                cmd,
                name,
                cwd,
                bypass,
                direction,
            },
        ),
        Cmd::Split => spawn::split(&ctx),
        Cmd::Send {
            message,
            target,
            wait,
            from,
        } => send::send(&ctx, &message, &target, wait, from.as_ref()),
        Cmd::Read { target, lines } => read::read(&ctx, &target, lines),
        Cmd::Wait {
            target,
            until,
            timeout,
        } => wait::wait(&ctx, &target, until, Duration::from_secs(timeout)),
        Cmd::Close { target } => close::close(&ctx, &target),
        Cmd::Telegram { cmd } => telegram(&ctx, cmd),
        Cmd::Schedule { cmd } => match cmd {
            SchedCmd::List => schedule::list(&ctx),
            SchedCmd::AddMessage { id, cron, tz, limits, target, message } => {
                schedule::add_message(&ctx, &id, &cron, &tz, &limits, &target, &message)
            }
            SchedCmd::AddCommand { id, cron, tz, notify, limits, command } => {
                schedule::add_command(&ctx, &id, &cron, &tz, notify.as_deref(), &limits, &command)
            }
            SchedCmd::Rm { id } => schedule::rm(&ctx, &id),
            SchedCmd::Pause { id, when, tz } => schedule::pause(&ctx, &id, &when, &tz),
            SchedCmd::Resume { id } => schedule::resume(&ctx, &id),
            SchedCmd::Enable { id } => schedule::enable(&ctx, &id, true),
            SchedCmd::Disable { id } => schedule::enable(&ctx, &id, false),
        },
    }
}

/// Real Bot API client for a token. If the HTTP client cannot be built, every
/// call reports why (the message never contains the token).
fn http_api(token: &str) -> Box<dyn TelegramApi> {
    match HttpTelegram::new(token) {
        Ok(api) => Box::new(api),
        Err(e) => Box::new(FailingTelegram(format!("{e:#}"))),
    }
}

fn saved_token(ctx: &Ctx) -> Result<String> {
    let cfg: TelegramConfig = ctx.state.read_json(TELEGRAM_FILE)?;
    match cfg.token {
        Some(t) => Ok(t),
        None => bail!("telegram is not connected. Run: colony telegram connect <token>"),
    }
}

fn telegram(ctx: &Ctx, cmd: TgCmd) -> Result<String> {
    match cmd {
        TgCmd::Connect { token, new_pane } => {
            let exe = std::env::current_exe()?;
            let done = cmds::connect(ctx, &http_api, &token, &exe, new_pane)?;
            if done.run_here {
                println!("{}", done.message);
                bot::run(ctx, &http_api)
            }
            Ok(done.message)
        }
        TgCmd::Disconnect => cmds::disconnect(ctx),
        TgCmd::Status => cmds::status(ctx, &http_api),
        TgCmd::Receiver { name, clear } => cmds::receiver_pinned(ctx, &http_api, name.as_deref(), clear),
        TgCmd::Reply { reply_id, text, photo, file } => {
            let attach = match (photo, file) {
                (Some(path), _) => Some(cmds::Attachment { kind: FileKind::Photo, path }),
                (None, Some(path)) => Some(cmds::Attachment { kind: FileKind::Document, path }),
                (None, None) => None,
            };
            // Straight to the Bot API from the saved token: no bot process, no receiver.
            let api = http_api(&saved_token(ctx)?);
            cmds::reply_with(ctx, api.as_ref(), &reply_id, text.as_deref(), attach.as_ref())
        }
        TgCmd::Run => {
            saved_token(ctx)?;
            bot::run(ctx, &http_api)
        }
    }
}
