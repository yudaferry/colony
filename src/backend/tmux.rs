//! tmux backend, matching the argv of the old Bash script (old-colony/colony).
//! Free text is passed as one argv element after `--` (verified live: `--` is
//! required when the text starts with `-`).

use super::runner::sv;
use super::{
    AgentState, Backend, BackendError, BackendKind, Direction, Kind, PaneInfo, PaneRef, Runner,
};
use anyhow::{Result, anyhow};
use std::path::Path;
use std::time::Duration;

const TMUX: &str = "tmux";
const SEND_PAUSE: Duration = Duration::from_millis(300);
/// D-5: wait up to 30 s (60 x 0.5 s) for the pane to leave the shell.
const START_POLLS: u32 = 60;
const START_POLL_PAUSE: Duration = Duration::from_millis(500);
const TUI_RENDER_PAUSE: Duration = Duration::from_secs(1);
/// After the pane leaves the shell, look at the screen up to 10 x 0.5 s = 5 s.
const DIALOG_POLLS: u32 = 10;

/// Startup dialogs, matched on lowercased screen text. These are the dialogs' own
/// wording, not bare words: the normal footer of a `--dangerously-skip-permissions`
/// session says "bypass permissions on" and must not match.
/// - folder trust: "do you trust the files in this folder" (or "trust this folder")
/// - bypass warning: "bypass permissions mode" together with its choices
///   "yes, i accept" / "no, exit"
fn startup_dialog(low: &str) -> Option<&'static str> {
    if low.contains("bypass permissions mode")
        && (low.contains("yes, i accept") || low.contains("no, exit"))
    {
        return Some("bypass-permissions prompt");
    }
    if low.contains("do you trust the files in this folder") || low.contains("trust this folder") {
        return Some("folder-trust prompt");
    }
    None
}

/// A Claude screen (lowercased text), also in a narrow pane where the footer is cut to
/// `⏵⏵ bypass`: any footer marker, the banner, or the prompt box (a `❯` line next to a
/// horizontal rule). Never true on a startup dialog, which also draws a `❯` and rules.
pub fn claude_ready(low: &str) -> bool {
    if startup_dialog(low).is_some() {
        return false;
    }
    let marker = [
        "? for shortcuts",
        "bypass permissions on",
        "accept edits on",
        "plan mode on",
        "shift+tab to cycle",
        "\u{23f5}\u{23f5}",
        "claude code v",
    ]
    .iter()
    .any(|m| low.contains(m));
    let prompt_box = low.lines().any(|l| l.trim_start().starts_with('\u{276f}'))
        && low.lines().any(|l| l.matches('\u{2500}').count() >= 5);
    marker || prompt_box
}

pub struct Tmux<R: Runner> {
    runner: R,
    env_pane: Option<String>,
    /// Injectable so tests do not wait.
    sleep: Box<dyn Fn(Duration)>,
}

impl<R: Runner> Tmux<R> {
    pub fn new(runner: R, env_pane: Option<String>) -> Self {
        Self::with_sleep(runner, env_pane, Box::new(std::thread::sleep))
    }

    pub fn with_sleep(runner: R, env_pane: Option<String>, sleep: Box<dyn Fn(Duration)>) -> Self {
        Self {
            runner,
            env_pane,
            sleep,
        }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    fn tmux(&self, args: &[&str]) -> Result<String> {
        self.runner.run(TMUX, &sv(args))
    }

    /// OpenCode content sniff (D-10); a failed capture counts as "not OpenCode".
    fn looks_like_opencode(&self, pane: &str) -> bool {
        match self.tmux(&["capture-pane", "-p", "-t", pane]) {
            Ok(content) => {
                content.contains("OpenCode")
                    || content.contains("ctrl+p commands")
                    || content.contains("tab agents")
            }
            Err(_) => false,
        }
    }

    /// Old `wait_for_pane`: until the pane no longer runs a shell, then a
    /// short pause for the TUI to render. Proceeds after the timeout (warning).
    fn wait_for_pane(&self, pane: &str) {
        for _ in 0..START_POLLS {
            if let Ok(Some(cmd)) = self.pane_command(pane)
                && !is_shell(&cmd)
            {
                (self.sleep)(TUI_RENDER_PAUSE);
                return;
            }
            (self.sleep)(START_POLL_PAUSE);
        }
        eprintln!("Warning: pane {pane} still at shell after 30s, sending anyway");
    }
}

fn is_shell(cmd: &str) -> bool {
    matches!(cmd, "zsh" | "bash" | "sh")
}

/// Always single-quotes `s` for a POSIX shell.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// D-6: keep the pane alive after the program exits.
pub fn wrap_cmd(cmd: &str) -> String {
    format!("zsh -c {}", shell_quote(&format!("{cmd}; exec zsh")))
}

fn claude_cmd(name: &str, bypass: bool) -> String {
    let mut cmd = format!("claude -n {}", shell_quote(name));
    if bypass {
        cmd.push_str(" --dangerously-skip-permissions");
    }
    cmd
}

impl<R: Runner> Backend for Tmux<R> {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn current_pane(&self) -> Result<String> {
        if let Some(p) = &self.env_pane {
            return Ok(p.clone());
        }
        // No -t: tmux falls back to the session's ACTIVE pane (old script comment).
        Ok(self.tmux(&["display-message", "-p", "#{pane_id}"])?.trim().to_string())
    }

    fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        let out = self.tmux(&["list-panes", "-a", "-F", "#{pane_id}"])?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|id| PaneInfo {
                pane_id: id.to_string(),
                agent: None,
            })
            .collect())
    }

    fn split(&self, from_pane: &str, dir: Direction, cwd: Option<&Path>) -> Result<String> {
        let flag = match dir {
            Direction::Right => "-h",
            Direction::Down => "-v",
        };
        let cwd = cwd.map(|p| p.to_string_lossy().into_owned());
        let mut args = vec![
            "split-window", flag, "-d", "-P", "-F", "#{pane_id}", "-t", from_pane,
        ];
        if let Some(c) = cwd.as_deref() {
            args.push("-c");
            args.push(c);
        }
        Ok(self.tmux(&args)?.trim().to_string())
    }

    fn split_column(&self, lead: &str, column: &[String], cwd: Option<&Path>) -> Result<String> {
        let cwd = cwd.map(|p| p.to_string_lossy().into_owned());
        let (target, flag, size): (&str, &str, &[&str]) = match column.last() {
            None => (lead, "-h", &["-l", "50%"]),
            Some(last) => (last.as_str(), "-v", &[]),
        };
        let mut args = vec!["split-window", flag, "-d", "-P", "-F", "#{pane_id}", "-t", target];
        args.extend_from_slice(size);
        if let Some(c) = cwd.as_deref() {
            args.push("-c");
            args.push(c);
        }
        let new = self.tmux(&args)?.trim().to_string();
        if !column.is_empty() {
            // Spreads the new pane and its vertical siblings (the column) evenly; the
            // lead is in another branch of the layout tree and is not touched.
            // Best effort: the pane exists already.
            let _ = self.tmux(&["select-layout", "-E", "-t", new.as_str()]);
        }
        Ok(new)
    }

    /// The new pane from `split` holds a plain shell; `respawn-pane -k` replaces
    /// it with the wrapped command (the pane keeps its start directory).
    fn run_program(&self, pane: &str, cmd: &str) -> Result<()> {
        let wrapped = wrap_cmd(cmd);
        self.tmux(&["respawn-pane", "-k", "-t", pane, wrapped.as_str()]).map(|_| ())
    }

    fn start_claude(&self, pane: &str, name: &str, bypass: bool) -> Result<()> {
        self.run_program(pane, &claude_cmd(name, bypass))?;
        self.wait_for_pane(pane);
        // Never type into a startup dialog: poll the screen until a dialog or a
        // ready Claude screen shows. A dialog, or no ready screen in time, fails closed.
        for i in 0..DIALOG_POLLS {
            let screen = self.tmux(&["capture-pane", "-p", "-t", pane])?;
            let low = screen.to_lowercase();
            if let Some(what) = startup_dialog(&low) {
                return Err(anyhow!(
                    "Claude in pane {pane} is waiting for you to accept the {what}; \
                     accept it there, then run: colony register {name} {pane}"
                ));
            }
            if claude_ready(&low) {
                return Ok(());
            }
            if i + 1 < DIALOG_POLLS {
                (self.sleep)(START_POLL_PAUSE);
            }
        }
        Err(anyhow!(
            "Claude in pane {pane} did not show a ready screen within {}s; nothing was typed. \
             Check the pane, then run: colony register {name} {pane}",
            DIALOG_POLLS as u64 * START_POLL_PAUSE.as_millis() as u64 / 1000
        ))
    }

    fn name_agent(&self, _pane: &str, _name: &str) -> Result<()> {
        Ok(())
    }

    fn send(&self, target: &PaneRef, text: &str) -> Result<()> {
        let pane = target.pane_id.as_str();
        if target.kind != Kind::Claude && self.looks_like_opencode(pane) {
            self.tmux(&["set-buffer", "--", text])?;
            self.tmux(&["paste-buffer", "-p", "-t", pane])?;
        } else {
            self.tmux(&["send-keys", "-l", "-t", pane, "--", text])?;
        }
        (self.sleep)(SEND_PAUSE);
        self.tmux(&["send-keys", "-t", pane, "Enter"]).map(|_| ())
    }

    fn read(&self, target: &PaneRef, lines: usize) -> Result<String> {
        let start = format!("-{lines}");
        let out = self.tmux(&["capture-pane", "-p", "-t", target.pane_id.as_str(), "-S", start.as_str()])?;
        let all: Vec<&str> = out.lines().collect();
        // capture-pane pads the visible area with blank lines; drop the trailing ones.
        let end = all.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
        let kept = &all[..end];
        let from = kept.len().saturating_sub(lines);
        Ok(kept[from..].join("\n"))
    }

    fn wait(&self, _target: &PaneRef, _until: AgentState, _timeout: Duration) -> Result<()> {
        Err(BackendError::HerdrOnly.into())
    }

    fn close(&self, pane: &str) -> Result<()> {
        self.tmux(&["kill-pane", "-t", pane]).map(|_| ())
    }

    fn agent_status(&self, _target: &PaneRef) -> Result<Option<AgentState>> {
        Ok(None)
    }

    fn screen_is_claude(&self, pane: &str) -> bool {
        self.tmux(&["capture-pane", "-p", "-t", pane])
            .is_ok_and(|s| claude_ready(&s.to_lowercase()))
    }

    fn pane_command(&self, pane: &str) -> Result<Option<String>> {
        let out = self.tmux(&["display-message", "-t", pane, "-p", "#{pane_current_command}"])?;
        let cmd = out.trim();
        Ok(if cmd.is_empty() { None } else { Some(cmd.to_string()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeRunner;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    fn tmux() -> Tmux<FakeRunner> {
        Tmux::with_sleep(FakeRunner::new(), Some("%1".to_string()), Box::new(|_| {}))
    }

    fn target(kind: Kind, pane: &str) -> PaneRef {
        PaneRef {
            backend: BackendKind::Tmux,
            pane_id: pane.to_string(),
            name: Some("bo".to_string()),
            kind,
        }
    }

    #[test]
    fn split_right_is_h() {
        let t = tmux();
        t.runner().push_ok("%5\n");
        assert_eq!(t.split("%1", Direction::Right, None).unwrap(), "%5");
        assert_eq!(
            t.runner().calls(),
            vec![sv(&[
                "tmux", "split-window", "-h", "-d", "-P", "-F", "#{pane_id}", "-t", "%1"
            ])]
        );
    }

    #[test]
    fn split_column_first_is_h_50_from_lead() {
        let t = tmux();
        t.runner().push_ok("%5\n");
        assert_eq!(t.split_column("%1", &[], None).unwrap(), "%5");
        assert_eq!(
            t.runner().calls(),
            vec![sv(&[
                "tmux", "split-window", "-h", "-d", "-P", "-F", "#{pane_id}", "-t", "%1", "-l", "50%"
            ])]
        );
    }

    #[test]
    fn split_column_next_is_v_below_last_then_equalised() {
        let t = tmux();
        t.runner().push_ok("%7\n");
        let col = vec!["%5".to_string(), "%6".to_string()];
        assert_eq!(t.split_column("%1", &col, None).unwrap(), "%7");
        assert_eq!(
            t.runner().calls(),
            vec![
                sv(&["tmux", "split-window", "-v", "-d", "-P", "-F", "#{pane_id}", "-t", "%6"]),
                sv(&["tmux", "select-layout", "-E", "-t", "%7"]),
            ]
        );
    }

    #[test]
    fn split_down_is_v() {
        let t = tmux();
        t.runner().push_ok("%6\n");
        t.split("%1", Direction::Down, None).unwrap();
        assert_eq!(t.runner().calls()[0][2], "-v");
    }

    #[test]
    fn split_with_cwd_passes_c() {
        let t = tmux();
        t.runner().push_ok("%7\n");
        let dir = PathBuf::from("/x/y");
        t.split("%1", Direction::Right, Some(&dir)).unwrap();
        let call = &t.runner().calls()[0];
        assert_eq!(&call[call.len() - 2..], &sv(&["-c", "/x/y"])[..]);
    }

    #[test]
    fn spawn_cmd_is_wrapped_in_zsh() {
        let t = tmux();
        t.run_program("%5", "htop").unwrap();
        assert_eq!(
            t.runner().calls(),
            vec![sv(&["tmux", "respawn-pane", "-k", "-t", "%5", "zsh -c 'htop; exec zsh'"])]
        );
        assert_eq!(wrap_cmd("echo 'a b'"), "zsh -c 'echo '\\''a b'\\''; exec zsh'");
    }

    #[test]
    fn start_claude_runs_wrapped_claude_then_waits_for_non_shell() {
        let slept = Rc::new(RefCell::new(Vec::new()));
        let rec = slept.clone();
        let t = Tmux::with_sleep(
            FakeRunner::new(),
            None,
            Box::new(move |d| rec.borrow_mut().push(d)),
        );
        t.runner().push_ok(""); // respawn-pane
        t.runner().push_ok("zsh\n"); // still a shell
        t.runner().push_ok("claude\n"); // started
        t.runner().push_ok("> \n  ? for shortcuts\n"); // capture-pane: ready screen
        t.start_claude("%5", "bo", true).unwrap();
        let calls = t.runner().calls();
        assert_eq!(
            calls[0],
            sv(&[
                "tmux", "respawn-pane", "-k", "-t", "%5",
                "zsh -c 'claude -n '\\''bo'\\'' --dangerously-skip-permissions; exec zsh'"
            ])
        );
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[3], sv(&["tmux", "capture-pane", "-p", "-t", "%5"]));
        assert_eq!(
            *slept.borrow(),
            vec![START_POLL_PAUSE, TUI_RENDER_PAUSE]
        );
    }

    fn dialog_start(screen: &str) -> Result<()> {
        let t = tmux();
        t.runner().push_ok(""); // respawn-pane
        t.runner().push_ok("claude\n");
        t.runner().push_ok(screen);
        let r = t.start_claude("%5", "bo", false);
        assert!(!t.runner().calls().iter().any(|c| c.contains(&"send-keys".to_string())));
        r
    }

    #[test]
    fn truncated_footer_is_ready() {
        let screen = "\u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\n\
            \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} bo \u{2500}\n\
            \u{276f} Try \"re\n\
            \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\
            colony git:(ma\u{2026}\n\
            \u{23f5}\u{23f5} bypass\n";
        let t = tmux();
        t.runner().push_ok(""); // respawn-pane
        t.runner().push_ok("claude\n");
        t.runner().push_ok(screen);
        t.start_claude("%5", "bo", true).unwrap();
        assert!(!t.runner().calls().iter().any(|c| c.contains(&"send-keys".to_string())));
        assert!(claude_ready("\u{23f5}\u{23f5} bypass"));
        assert!(claude_ready("\u{276f} try\n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}"));
    }

    #[test]
    fn dialog_with_prompt_glyph_is_not_ready() {
        let s = "do you trust the files in this folder?\n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\u{276f} 1. yes\n";
        assert!(!claude_ready(s));
        assert!(!claude_ready("user@host \u{276f} ls\n"));
    }

    #[test]
    fn screen_is_claude_captures_pane() {
        let t = tmux();
        t.runner().push_ok("\u{23f5}\u{23f5} bypass\n");
        assert!(t.screen_is_claude("%5"));
        t.runner().push_ok("$ ls\n");
        assert!(!t.screen_is_claude("%5"));
        assert_eq!(t.runner().calls()[0], sv(&["tmux", "capture-pane", "-p", "-t", "%5"]));
    }

    #[test]
    fn bypass_footer_alone_is_not_a_dialog() {
        let t = tmux();
        t.runner().push_ok(""); // respawn-pane
        t.runner().push_ok("claude\n");
        t.runner().push_ok("> \n  bypass permissions on (shift+tab to cycle)\n");
        t.start_claude("%5", "bo", true).unwrap();
    }

    #[test]
    fn dialog_on_second_poll_errors_without_typing() {
        let t = tmux();
        t.runner().push_ok(""); // respawn-pane
        t.runner().push_ok("claude\n");
        t.runner().push_ok(""); // blank first capture
        t.runner().push_ok("Do you trust the files in this folder?\n1. Yes\n");
        let e = t.start_claude("%5", "bo", false).unwrap_err();
        assert!(format!("{e:#}").contains("colony register bo %5"));
        assert!(!t.runner().calls().iter().any(|c| c.contains(&"send-keys".to_string())));
    }

    #[test]
    fn start_claude_trust_dialog_errors_without_typing() {
        let e = dialog_start("Do you trust the files in this folder?\n1. Yes\n").unwrap_err();
        let m = format!("{e:#}");
        assert!(m.contains("Claude in pane %5 is waiting for you to accept"), "{m}");
        assert!(m.contains("colony register bo %5"), "{m}");
    }

    #[test]
    fn start_claude_bypass_dialog_errors_without_typing() {
        let e = dialog_start("WARNING: Bypass Permissions mode\n1. No, exit\n2. Yes, I accept\n").unwrap_err();
        assert!(format!("{e:#}").contains("colony register bo %5"));
    }

    #[test]
    fn send_plain_uses_send_keys_literal_then_enter() {
        let t = tmux();
        t.send(&target(Kind::Claude, "%5"), "hello world").unwrap();
        assert_eq!(
            t.runner().calls(),
            vec![
                sv(&["tmux", "send-keys", "-l", "-t", "%5", "--", "hello world"]),
                sv(&["tmux", "send-keys", "-t", "%5", "Enter"]),
            ]
        );
    }

    #[test]
    fn send_opencode_uses_paste_buffer() {
        let t = tmux();
        t.runner().push_ok("OpenCode ... ctrl+p commands\n"); // capture-pane
        t.send(&target(Kind::Other, "%5"), "hi").unwrap();
        assert_eq!(
            t.runner().calls(),
            vec![
                sv(&["tmux", "capture-pane", "-p", "-t", "%5"]),
                sv(&["tmux", "set-buffer", "--", "hi"]),
                sv(&["tmux", "paste-buffer", "-p", "-t", "%5"]),
                sv(&["tmux", "send-keys", "-t", "%5", "Enter"]),
            ]
        );
    }

    #[test]
    fn send_shell_without_opencode_uses_literal_keys() {
        let t = tmux();
        t.runner().push_ok("user@host $ \n"); // capture-pane
        t.send(&target(Kind::Shell, "%5"), "ls").unwrap();
        let calls = t.runner().calls();
        assert_eq!(calls[1], sv(&["tmux", "send-keys", "-l", "-t", "%5", "--", "ls"]));
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn read_limits_to_n_lines() {
        let t = tmux();
        t.runner().push_ok("1\n2\n3\n4\n5\n\n\n");
        assert_eq!(t.read(&target(Kind::Claude, "%5"), 2).unwrap(), "4\n5");
        assert_eq!(
            t.runner().calls(),
            vec![sv(&["tmux", "capture-pane", "-p", "-t", "%5", "-S", "-2"])]
        );
    }

    #[test]
    fn wait_is_herdr_only() {
        let t = tmux();
        let err = t
            .wait(&target(Kind::Claude, "%5"), AgentState::Idle, Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(err.to_string(), "herdr only");
        assert!(matches!(err.downcast_ref::<BackendError>(), Some(BackendError::HerdrOnly)));
    }

    #[test]
    fn agent_status_is_none() {
        let t = tmux();
        assert_eq!(t.agent_status(&target(Kind::Claude, "%5")).unwrap(), None);
        assert!(t.runner().calls().is_empty());
    }

    #[test]
    fn close_and_list_and_current_argv() {
        let t = tmux();
        t.runner().push_ok("%1\n%2\n");
        let panes = t.list_panes().unwrap();
        assert_eq!(panes.iter().map(|p| p.pane_id.as_str()).collect::<Vec<_>>(), ["%1", "%2"]);
        t.close("%2").unwrap();
        assert_eq!(
            t.runner().calls(),
            vec![
                sv(&["tmux", "list-panes", "-a", "-F", "#{pane_id}"]),
                sv(&["tmux", "kill-pane", "-t", "%2"]),
            ]
        );
        assert_eq!(t.current_pane().unwrap(), "%1");
    }

    #[test]
    fn current_pane_falls_back_to_display_message() {
        let t = Tmux::with_sleep(FakeRunner::new(), None, Box::new(|_| {}));
        t.runner().push_ok("%9\n");
        assert_eq!(t.current_pane().unwrap(), "%9");
        assert_eq!(
            t.runner().calls(),
            vec![sv(&["tmux", "display-message", "-p", "#{pane_id}"])]
        );
    }

    #[test]
    fn tmux_send_passes_tricky_text_verbatim() {
        let text = "-x \"q\" $HOME `id`\nline2";
        let t = tmux();
        t.send(&target(Kind::Claude, "%5"), text).unwrap();
        let call = &t.runner().calls()[0];
        assert_eq!(call.last().unwrap(), text);
        // `--` comes right before the text so a leading `-` is not a flag.
        assert_eq!(call[call.len() - 2], "--");
    }
}
