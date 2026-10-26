//! Leak check: after teardown, prove that a run left nothing behind.
//!
//! ```no_run
//! use firestream_e2e_core::{env::env_keep, leak, run_id::RunId};
//!
//! // after the guard dropped:
//! leak::check(RunId::current(), Some("redis"), env_keep())?;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! The check uses the same filters as the sweeper ([`crate::sweep`]), so
//! anything it reports is something `firestream-e2e-sweep --run <id>`
//! removes.

use std::fmt;
use std::path::Path;

use crate::run_id::RunId;
use crate::runner::{CommandRunner, SystemRunner};
use crate::sweep::{Scope, discover};

/// Objects that survived teardown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaks {
    pub run: RunId,
    pub target: Option<String>,
    /// One line per object (`"k3d network k3d-firestream-e2e-…"`).
    pub objects: Vec<String>,
    /// Listings that failed, so the check could not prove the host clean.
    pub errors: Vec<String>,
}

impl fmt::Display for Leaks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match &self.target {
            Some(t) => format!("run {} (target {t})", self.run),
            None => format!("run {}", self.run),
        };
        if !self.objects.is_empty() {
            writeln!(f, "e2e leak check failed for {what}: {} object(s) remain:", self.objects.len())?;
            for o in &self.objects {
                writeln!(f, "  - {o}")?;
            }
        }
        if !self.errors.is_empty() {
            writeln!(f, "e2e leak check for {what} could not list every object:")?;
            for e in &self.errors {
                writeln!(f, "  - {e}")?;
            }
        }
        write!(f, "Remove them with: {}", sweep_command(&self.run))
    }
}

impl std::error::Error for Leaks {}

/// The command an operator runs to remove everything of `run`.
pub fn sweep_command(run: &RunId) -> String {
    format!(
        "make e2e-sweep RUN={run}  (or: cargo run -p firestream-e2e-core --bin firestream-e2e-sweep -- --run {run})"
    )
}

/// Check that no object of `run` (only of `target`, if given) remains.
///
/// With `keep` set (`FIRESTREAM_E2E_KEEP=1`, or a backend's own keep var)
/// the resources are left on purpose: the check is skipped and the sweep
/// command is printed.
///
/// A missing `k3d` or `kubectl` is not an error; a failed `docker` listing
/// is, because then the host cannot be proven clean.
pub fn check_with(
    runner: &dyn CommandRunner,
    run: &RunId,
    target: Option<&str>,
    tmp_dir: &Path,
    keep: bool,
) -> Result<(), Leaks> {
    if keep {
        eprintln!(
            "[e2e] KEEP=1: leak check skipped; resources of run {run} stay. Clean up with `make e2e-sweep RUN={run}`"
        );
        return Ok(());
    }
    let inv = discover(runner, &Scope::for_run(run, target), tmp_dir);
    if inv.is_empty() && inv.errors.is_empty() {
        return Ok(());
    }
    Err(Leaks {
        run: run.clone(),
        target: target.map(String::from),
        objects: inv.objects(),
        errors: inv.errors,
    })
}

/// [`check_with`] against the real host and `std::env::temp_dir()`.
pub fn check(run: &RunId, target: Option<&str>, keep: bool) -> Result<(), Leaks> {
    check_with(&SystemRunner, run, target, &std::env::temp_dir(), keep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CmdOutput;
    use crate::sweep::fake::{FakeRunner, busy_host};

    fn empty_tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fs-e2e-core-leak-test-{}", RunId::random()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn clean_run_passes() {
        let tmp = empty_tmp();
        let run = RunId::parse("nnn000").unwrap();
        assert!(check_with(&busy_host(), &run, None, &tmp, false).is_ok());
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn leak_lists_objects_and_sweep_command() {
        let tmp = empty_tmp();
        let run = RunId::parse("abc123").unwrap();
        let err = check_with(&busy_host(), &run, Some("redis"), &tmp, false).unwrap_err();
        let text = err.to_string();
        assert!(text.starts_with("e2e leak check failed for run abc123 (target redis): 9 object(s) remain:"), "{text}");
        assert!(text.contains("  - k3d network k3d-firestream-e2e-redis-abc123\n"), "{text}");
        assert!(text.contains("  - kubeconfig context k3d-firestream-e2e-redis-abc123\n"), "{text}");
        assert!(text.contains("make e2e-sweep RUN=abc123"), "{text}");
        assert!(!text.contains("rkpzmb"), "{text}");

        let other_target = check_with(&busy_host(), &run, Some("kafka"), &tmp, false);
        assert!(other_target.is_ok());
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn keep_skips_the_check() {
        let tmp = empty_tmp();
        let runner = busy_host();
        let run = RunId::parse("abc123").unwrap();
        assert!(check_with(&runner, &run, None, &tmp, true).is_ok());
        assert!(runner.calls().is_empty());
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn docker_failure_fails_the_check() {
        let tmp = empty_tmp();
        let runner = FakeRunner::default().reply("docker ", CmdOutput::failed("Cannot connect to the Docker daemon"));
        let err = check_with(&runner, &RunId::parse("abc123").unwrap(), None, &tmp, false).unwrap_err();
        assert!(err.objects.is_empty());
        assert!(err.to_string().contains("could not list every object"));
        let _ = std::fs::remove_dir_all(tmp);
    }
}
