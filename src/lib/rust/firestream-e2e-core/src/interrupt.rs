//! Interrupt tests: kill a harness run with SIGTERM during its setup and
//! prove that it leaves nothing behind.
//!
//! A test binary holds two `#[ignore]` tests. The child test does real
//! harness work (create a cluster, bring a compose stack up) and returns at
//! once unless [`is_child`] is true. The parent test calls [`run`], which
//! re-runs the child test in a new process under a fresh run id, waits until
//! the first docker object of that run exists, sends SIGTERM, and then checks the
//! host for leftovers.
//!
//! ```no_run
//! use firestream_e2e_core::interrupt;
//!
//! #[test]
//! #[ignore]
//! fn child() {
//!     if !interrupt::is_child() {
//!         return;
//!     }
//!     // harness setup that takes long enough to be interrupted
//! }
//!
//! #[test]
//! #[ignore]
//! fn sigterm_during_setup_leaves_nothing() {
//!     let report = interrupt::run("child", "redis", &[]).expect("interrupt run");
//!     report.assert_clean();
//! }
//! ```

use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::guard::{POLL, TEARDOWN_BUDGET};
use crate::leak::{self, Leaks};
use crate::lock::HostLock;
use crate::run_id::{RUN_ID_ENV, RunId};
use crate::runner::SystemRunner;
use crate::sweep::{Scope, sweep};

/// Set to `1` in the child process.
pub const CHILD_ENV: &str = "FIRESTREAM_E2E_INTERRUPT_CHILD";

/// The longest wait for the child to create its first object. It covers a
/// cold image build before `docker compose up`.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(1800);

/// After the child exits, wait this long and check again. A k3d or compose
/// subprocess that outlived the child would create its objects in this
/// window.
pub const SETTLE: Duration = Duration::from_secs(30);

const KEEP_VARS: [&str; 2] = ["FIRESTREAM_E2E_KEEP", "FIRESTREAM_E2E_K8S_KEEP"];

/// True in the child process of an interrupt test.
pub fn is_child() -> bool {
    std::env::var(CHILD_ENV).ok().as_deref() == Some("1")
}

/// What the parent saw.
#[derive(Debug)]
pub struct Report {
    pub run: RunId,
    pub status: ExitStatus,
    /// Seconds from spawn to SIGTERM.
    pub signalled_after: Duration,
    /// The leak check right after the child exited.
    pub at_exit: Result<(), Leaks>,
    /// The leak check after [`SETTLE`].
    pub after_settle: Result<(), Leaks>,
}

impl Report {
    /// Panic unless the child exited 143 and both leak checks passed.
    pub fn assert_clean(&self) {
        let mut problems = Vec::new();
        if self.status.code() != Some(143) {
            problems.push(format!("child exit status {} (want 143)", self.status));
        }
        if let Err(l) = &self.at_exit {
            problems.push(format!("at child exit: {l}"));
        }
        if let Err(l) = &self.after_settle {
            problems.push(format!("after {SETTLE:?}: {l}"));
        }
        assert!(problems.is_empty(), "interrupt of run {} was not clean:\n{}", self.run, problems.join("\n"));
    }
}

/// Re-run `child_test` of this test binary under a fresh run id, send it
/// SIGTERM as soon as an object of `target` in that run exists, and check
/// for leftovers.
///
/// The caller must not hold the [`HostLock`]: the child takes it. Whatever
/// the result, objects of the run that remain are swept afterwards, so a
/// failed test still leaves a clean host.
pub fn run(child_test: &str, target: &str, envs: &[(&str, &str)]) -> Result<Report> {
    let run = RunId::random();
    let start = Instant::now();
    let mut child = spawn(child_test, &run, envs)?;
    let _sweep_after = SweepAfter(run.clone());

    eprintln!("[interrupt] run {run}: child {} started; waiting for the first object of {target}", child.id());
    loop {
        if let Some(status) = child.try_wait().context("poll child")? {
            bail!("child exited {status} before it created any object of run {run}");
        }
        if any_docker_object(&run.resource_name(target)) {
            break;
        }
        if start.elapsed() > SETUP_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("no object of run {run} appeared within {SETUP_TIMEOUT:?}");
        }
        thread::sleep(POLL);
    }

    let signalled_after = start.elapsed();
    eprintln!("[interrupt] run {run}: first object seen after {signalled_after:?}; sending SIGTERM");
    sigterm(&child)?;
    let status = wait_exit(&mut child)?;
    eprintln!("[interrupt] run {run}: child exited {status}");

    let at_exit = leak::check(&run, None, false);
    thread::sleep(SETTLE);
    let after_settle = leak::check(&run, None, false);
    Ok(Report {
        run,
        status,
        signalled_after,
        at_exit,
        after_settle,
    })
}

/// Is there a container, network or volume whose name holds `name`?
///
/// NOTE: this is the trigger, not the proof. A full [`leak::check`] pass
/// takes seconds, and a small compose stack is up and probed in less, so
/// the signal would land after setup.
fn any_docker_object(name: &str) -> bool {
    let filter = format!("name={name}");
    [
        &["ps", "-aq", "--filter", &filter][..],
        &["network", "ls", "-q", "--filter", &filter][..],
        &["volume", "ls", "-q", "--filter", &filter][..],
    ]
    .iter()
    .any(|args| {
        Command::new("docker")
            .args(*args)
            .output()
            .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
    })
}

fn spawn(child_test: &str, run: &RunId, envs: &[(&str, &str)]) -> Result<Child> {
    let exe = std::env::current_exe().context("current_exe")?;
    let mut cmd = Command::new(exe);
    cmd.args([child_test, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .env(RUN_ID_ENV, run.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for k in KEEP_VARS {
        cmd.env_remove(k);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.spawn().context("spawn child test process")
}

fn sigterm(child: &Child) -> Result<()> {
    let pid = libc::pid_t::try_from(child.id()).context("child pid")?;
    // SAFETY: kill(2) with a pid this process spawned and has not reaped.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        bail!("kill -TERM {pid}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// The child's drain runs each teardown under [`TEARDOWN_BUDGET`]. A k3d
/// run has one teardown, a compose run has the stack and the egress helper.
fn wait_exit(child: &mut Child) -> Result<ExitStatus> {
    let budget = TEARDOWN_BUDGET * 3;
    let start = Instant::now();
    while start.elapsed() < budget {
        if let Some(status) = child.try_wait().context("poll child")? {
            return Ok(status);
        }
        thread::sleep(POLL);
    }
    let _ = child.kill();
    let status = child.wait().context("reap child")?;
    bail!("child did not exit within {budget:?} of SIGTERM (killed: {status})")
}

struct SweepAfter(RunId);

impl Drop for SweepAfter {
    fn drop(&mut self) {
        let Ok(lock) = HostLock::acquire() else {
            eprintln!("[interrupt] run {}: cannot take the host lock; run `make e2e-sweep RUN={}`", self.0, self.0);
            return;
        };
        let (inv, _, report) = sweep(&SystemRunner, &Scope::for_run(&self.0, None), &std::env::temp_dir(), &lock, false);
        if !inv.is_empty() {
            eprintln!(
                "[interrupt] run {}: swept {} leftover object(s), {} step(s) failed",
                self.0,
                inv.objects().len(),
                report.failures().len()
            );
        }
    }
}
