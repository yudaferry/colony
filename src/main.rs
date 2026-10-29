use anyhow::{Result, bail};
use clap::{CommandFactory, Parser};
use colony::backend::RealBackends;
use colony::cli::{Cli, Cmd, TgCmd};
use colony::commands::{close, list, read, register, send, spawn, wait};
use colony::ctx::{Ctx, ProcessEnv, SystemClock};
use colony::state::store::StateDir;
use colony::state::telegram::{TELEGRAM_FILE, TelegramConfig};
use colony::telegram::api::{FailingTelegram, HttpTelegram, TelegramApi};
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
        TgCmd::Receiver { name, clear } => cmds::receiver(ctx, name.as_deref(), clear),
        TgCmd::Reply { reply_id, text } => {
            // Straight to the Bot API from the saved token: no bot process, no receiver.
            let api = http_api(&saved_token(ctx)?);
            cmds::reply(ctx, api.as_ref(), &reply_id, &text)
        }
        TgCmd::Run => {
            saved_token(ctx)?;
            bot::run(ctx, &http_api)
        }
    }
}
