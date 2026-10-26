//! The command seam for the sweeper and the leak check.
//!
//! Every `docker`, `k3d` and `kubectl` call in [`crate::sweep`] and
//! [`crate::leak`] goes through [`CommandRunner`], so unit tests drive them
//! with a fake that returns canned output and records the call order.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::guard::{TEARDOWN_BUDGET, wait_with_budget};

/// Captured result of one command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdOutput {
    /// Exit code; `None` if the process died from a signal or timed out.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Successful output with `stdout`.
    pub fn ok(stdout: impl Into<String>) -> Self {
        CmdOutput {
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// Failed output (exit 1) with `stderr`.
    pub fn failed(stderr: impl Into<String>) -> Self {
        CmdOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

/// Runs one external command to completion. `Err` means the command could
/// not start (for example, the binary is not on `PATH`).
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[String]) -> std::io::Result<CmdOutput>;
}

/// [`CommandRunner`] that spawns real processes, each bounded by
/// [`TEARDOWN_BUDGET`]. A command that runs past the budget is killed and
/// reported with `code: None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> std::io::Result<CmdOutput> {
        run_bounded(program, args, TEARDOWN_BUDGET)
    }
}

fn run_bounded(program: &str, args: &[String], budget: Duration) -> std::io::Result<CmdOutput> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    // NOTE: the pipes drain on their own threads; a child that fills a
    // pipe buffer while nobody reads would otherwise block until the budget.
    let out_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let code = match wait_with_budget(&mut child, budget) {
        Ok(()) => child.wait()?.code(),
        Err(()) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    };
    let stdout = out_reader.join().unwrap_or_default();
    let mut stderr = err_reader.join().unwrap_or_default();
    if code.is_none() && stderr.is_empty() {
        stderr = format!("{program} did not finish within {}s", budget.as_secs());
    }
    Ok(CmdOutput { code, stdout, stderr })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_runner_captures_output() {
        let out = SystemRunner
            .run("sh", &["-c".into(), "echo hi; echo err >&2; exit 3".into()])
            .unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.stderr, "err\n");
    }

    #[test]
    fn system_runner_reports_missing_binary() {
        assert!(SystemRunner.run("definitely-not-a-real-bin-xyzzy", &[]).is_err());
    }

    #[test]
    fn run_bounded_kills_on_timeout() {
        let out = run_bounded("sleep", &["5".into()], Duration::from_millis(300)).unwrap();
        assert_eq!(out.code, None);
        assert!(out.stderr.contains("did not finish"));
    }
}
