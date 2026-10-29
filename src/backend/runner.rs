//! Running external programs through an argv array (never through a shell).

use anyhow::{Context, Result, anyhow};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::process::{Command, Stdio};

/// Runs a program and returns its stdout. A non-zero exit becomes an `Err`
/// that carries stderr (and stdout, since herdr prints its JSON errors there).
pub trait Runner {
    fn run(&self, program: &str, args: &[String]) -> Result<String>;
}

/// Turns string slices into an owned argv vector.
pub fn sv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String> {
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("failed to run {program}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut detail = stderr.trim().to_string();
        let so = stdout.trim();
        if !so.is_empty() {
            if !detail.is_empty() {
                detail.push_str(" | ");
            }
            detail.push_str(so);
        }
        // Only the first two argv words are named: later ones may be message text.
        let head: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
        Err(anyhow!(
            "{program} {} failed ({}): {detail}",
            head.join(" "),
            out.status
        ))
    }
}

/// Test runner: records every call as `[program, args...]` and answers from a
/// queue of canned replies (an empty queue answers `Ok("")`).
#[derive(Default)]
pub struct FakeRunner {
    calls: RefCell<Vec<Vec<String>>>,
    replies: RefCell<VecDeque<Result<String>>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_ok(&self, stdout: &str) {
        self.replies.borrow_mut().push_back(Ok(stdout.to_string()));
    }

    pub fn push_err(&self, message: &str) {
        self.replies
            .borrow_mut()
            .push_back(Err(anyhow!(message.to_string())));
    }

    /// Every call so far, each as `[program, args...]`.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String> {
        let mut call = vec![program.to_string()];
        call.extend(args.iter().cloned());
        self.calls.borrow_mut().push(call);
        self.replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Ok(String::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_runner_records_argv() {
        let r = FakeRunner::new();
        r.push_ok("out");
        let got = r.run("tmux", &sv(&["send-keys", "-l", "a b"])).unwrap();
        assert_eq!(got, "out");
        assert_eq!(r.calls(), vec![sv(&["tmux", "send-keys", "-l", "a b"])]);
    }

    #[test]
    fn fake_runner_returns_queued_errors_then_empty_ok() {
        let r = FakeRunner::new();
        r.push_err("boom");
        assert!(r.run("x", &[]).unwrap_err().to_string().contains("boom"));
        assert_eq!(r.run("x", &[]).unwrap(), "");
    }
}
