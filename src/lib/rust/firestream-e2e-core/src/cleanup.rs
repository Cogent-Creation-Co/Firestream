//! Process-wide registry of pending teardowns, drained on SIGINT, SIGTERM
//! and SIGHUP.
//!
//! A teardown guard's `Drop` does not run when the process dies from a
//! signal, so without this registry a Ctrl-C leaks the run's cluster or
//! compose project. A guard registers its teardown when it is armed; the
//! signal handler runs every registered teardown, then exits with
//! `128 + signo` (130 for SIGINT, 143 for SIGTERM, 129 for SIGHUP).
//!
//! # Use from a guard
//!
//! ```no_run
//! use firestream_e2e_core::cleanup::{self, Registration};
//!
//! struct StackGuard {
//!     registration: Option<Registration>,
//! }
//!
//! impl StackGuard {
//!     fn arm(project: String) -> anyhow::Result<Self> {
//!         cleanup::install_signal_handler()?;
//!         let registration = cleanup::register(format!("compose project {project}"), move || {
//!             // docker compose -p <project> down -v --remove-orphans, under
//!             // guard::TEARDOWN_BUDGET
//!         });
//!         Ok(StackGuard { registration: Some(registration) })
//!     }
//! }
//!
//! impl Drop for StackGuard {
//!     fn drop(&mut self) {
//!         if let Some(r) = self.registration.take() {
//!             r.run();
//!         }
//!     }
//! }
//! ```
//!
//! Arm the guard *before* the create call, so a partial create is also
//! torn down.
//!
//! # Exactly once
//!
//! The registry owns each teardown closure. [`Registration::run`] and the
//! signal drain both take the closure out of the registry under one mutex,
//! so a teardown runs at most once, even if a signal arrives while a guard
//! drops during a panic unwind. If the guard already took the closure and is
//! still running it, the drain waits for it (up to
//! [`crate::guard::TEARDOWN_BUDGET`]) before the process exits.
//!
//! A second signal during the drain exits at once with the same code.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::guard::TEARDOWN_BUDGET;

type Teardown = Box<dyn FnOnce() + Send + 'static>;

struct Entry {
    label: String,
    teardown: Teardown,
}

struct State {
    next_id: u64,
    entries: BTreeMap<u64, Entry>,
    in_flight: BTreeMap<u64, String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    next_id: 0,
    entries: BTreeMap::new(),
    in_flight: BTreeMap::new(),
});
static CHANGED: Condvar = Condvar::new();

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Handle to one registered teardown.
///
/// - [`Registration::run`] runs the teardown now (normal guard drop).
/// - [`Registration::disarm`] or a plain drop removes it without running
///   it (for `KEEP=1`).
#[must_use = "dropping a Registration deregisters the teardown without running it"]
#[derive(Debug)]
pub struct Registration {
    id: u64,
}

/// Register `teardown` to run if the process receives SIGINT, SIGTERM or
/// SIGHUP before the returned [`Registration`] runs or drops. `label` names
/// the resource in log lines (`"k3d cluster firestream-e2e-redis-abc123"`).
///
/// The closure must bound its own run time by
/// [`crate::guard::TEARDOWN_BUDGET`]; the drain does not kill it, it only
/// stops waiting for it.
pub fn register<F>(label: impl Into<String>, teardown: F) -> Registration
where
    F: FnOnce() + Send + 'static,
{
    let mut s = state();
    let id = s.next_id;
    s.next_id += 1;
    s.entries.insert(
        id,
        Entry {
            label: label.into(),
            teardown: Box::new(teardown),
        },
    );
    Registration { id }
}

impl Registration {
    /// Run the teardown on this thread, unless the signal drain already
    /// took it. Returns once the teardown is done.
    pub fn run(self) {
        park_if_draining();
        let entry = {
            let mut s = state();
            let entry = s.entries.remove(&self.id);
            if let Some(e) = &entry {
                s.in_flight.insert(self.id, e.label.clone());
            }
            entry
        };
        if let Some(entry) = entry {
            run_tracked(self.id, entry.teardown);
        }
    }

    /// Remove the teardown without running it.
    pub fn disarm(self) {}
}

impl Drop for Registration {
    fn drop(&mut self) {
        park_if_draining();
        state().entries.remove(&self.id);
    }
}

/// Block this thread for good while a signal drain runs.
///
/// WARNING: do not return here. The drain ends the process with
/// `128 + signo`. A guard thread that goes on can reach the end of `main`
/// first; the process then exits with the test's code and cuts the drain
/// off in the middle of a teardown, which leaks the run's objects.
fn park_if_draining() {
    if DRAINING.load(Ordering::SeqCst) {
        loop {
            std::thread::park();
        }
    }
}

/// Run `teardown` and mark `id` finished even if it panics.
fn run_tracked(id: u64, teardown: Teardown) {
    struct Finish(u64);
    impl Drop for Finish {
        fn drop(&mut self) {
            state().in_flight.remove(&self.0);
            CHANGED.notify_all();
        }
    }
    let _finish = Finish(id);
    teardown();
}

/// Labels of the teardowns that are registered now and not yet started.
pub fn pending() -> Vec<String> {
    state().entries.values().map(|e| e.label.clone()).collect()
}

/// Outcome of [`drain`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Teardowns this drain started.
    pub started: Vec<String>,
    /// Teardowns (started here or by a guard) still running at the
    /// deadline.
    pub unfinished: Vec<String>,
}

/// Run every registered teardown, each on its own thread, and wait until
/// all of them, and any teardown a guard is running now, finish or
/// `budget` passes. Teardowns registered during the drain also run.
pub fn drain(budget: Duration) -> DrainReport {
    let deadline = Instant::now() + budget;
    let mut report = DrainReport::default();
    let mut s = state();
    loop {
        let entries = std::mem::take(&mut s.entries);
        for (id, entry) in entries {
            s.in_flight.insert(id, entry.label.clone());
            report.started.push(entry.label);
            let teardown = entry.teardown;
            let spawned = std::thread::Builder::new()
                .name(format!("e2e-teardown-{id}"))
                .spawn(move || run_tracked(id, teardown));
            if spawned.is_err() {
                s.in_flight.remove(&id);
            }
        }
        if s.in_flight.is_empty() && s.entries.is_empty() {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            report.unfinished = s.in_flight.values().cloned().collect();
            break;
        }
        s = CHANGED
            .wait_timeout(s, deadline - now)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
    report
}

static LAST_SIGNAL: AtomicI32 = AtomicI32::new(0);
static DRAINING: AtomicBool = AtomicBool::new(false);

/// Exit code for a death by `signal`: `128 + signo`. Unknown or zero means
/// SIGINT.
pub fn exit_code_for(signal: i32) -> i32 {
    if signal > 0 { 128 + signal } else { 128 + libc::SIGINT }
}

/// Install the SIGINT/SIGTERM/SIGHUP handler that drains the registry and
/// exits with [`exit_code_for`]. Safe to call many times; only the first
/// call installs it.
///
/// Fails if another part of the process already installed a `ctrlc`
/// handler.
pub fn install_signal_handler() -> Result<()> {
    static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| {
            ctrlc::set_handler(on_signal).map_err(|e| e.to_string())?;
            // NOTE: this must run after ctrlc::set_handler, because it
            // chains in front of the handlers that ctrlc installs.
            unsafe { signal_record::install() };
            Ok(())
        })
        .clone()
        .map_err(|e| anyhow!("install e2e signal handler: {e}"))
}

fn on_signal() {
    let code = exit_code_for(LAST_SIGNAL.load(Ordering::SeqCst));
    if DRAINING.swap(true, Ordering::SeqCst) {
        eprintln!("[e2e] second signal: exiting with {code} before teardown finished");
        std::process::exit(code);
    }
    // NOTE: the drain runs on its own thread so that the ctrlc thread stays
    // free to see a second signal and exit at once.
    let spawned = std::thread::Builder::new()
        .name("e2e-signal-drain".into())
        .spawn(move || {
            // NOTE: a teardown in flight has its own subprocesses (docker,
            // k3d delete); stopping them would cut that teardown short.
            if state().in_flight.is_empty() {
                descendants::stop(DESCENDANT_BUDGET);
            }
            let pending = pending();
            eprintln!(
                "[e2e] signal received: running {} teardown(s) before exit {code}",
                pending.len()
            );
            for label in &pending {
                eprintln!("[e2e]   teardown: {label}");
            }
            let report = drain(TEARDOWN_BUDGET);
            for label in &report.unfinished {
                eprintln!(
                    "[e2e] teardown did not finish within {}s: {label}",
                    TEARDOWN_BUDGET.as_secs()
                );
            }
            std::process::exit(code);
        });
    if spawned.is_err() {
        drain(TEARDOWN_BUDGET);
        std::process::exit(code);
    }
}

/// Upper bound to stop the subprocesses of an interrupted setup.
const DESCENDANT_BUDGET: Duration = Duration::from_secs(10);

/// Stop every subprocess of this process before the drain.
///
/// An interrupted setup still has its `k3d cluster create` or
/// `docker compose up` running. Without this, the teardown races it: k3d's
/// own rollback then fails on a network with active endpoints, and compose
/// creates containers after `down` returned.
#[cfg(target_os = "linux")]
mod descendants {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{Duration, Instant};

    /// `(pid, ppid, state)` of every process in `/proc`.
    fn processes() -> Vec<(i32, i32, char)> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|pid| {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                // NOTE: the command name in field 2 can hold spaces and
                // parentheses, so the fields start after the last ')'.
                let mut rest = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
                let state = rest.next()?.chars().next()?;
                let ppid = rest.next()?.parse().ok()?;
                Some((pid, ppid, state))
            })
            .collect()
    }

    /// Live (not zombie) descendants of `root`.
    pub(super) fn of(root: i32) -> BTreeSet<i32> {
        let procs = processes();
        let mut children: BTreeMap<i32, Vec<(i32, char)>> = BTreeMap::new();
        for (pid, ppid, state) in procs {
            children.entry(ppid).or_default().push((pid, state));
        }
        let mut out = BTreeSet::new();
        let mut todo = vec![root];
        while let Some(p) = todo.pop() {
            for &(c, state) in children.get(&p).map(Vec::as_slice).unwrap_or(&[]) {
                if out.insert(c) {
                    todo.push(c);
                }
                if state == 'Z' {
                    out.remove(&c);
                }
            }
        }
        out
    }

    fn signal_all(pids: &BTreeSet<i32>, signal: libc::c_int) {
        for &pid in pids {
            // SAFETY: kill(2) has no memory effects; a pid that exited in
            // the meantime gives ESRCH.
            unsafe { libc::kill(pid, signal) };
        }
    }

    /// [`stop_tree`] for this process.
    pub(super) fn stop(budget: Duration) {
        stop_tree(std::process::id() as i32, budget);
    }

    /// SIGTERM every descendant of `me`, wait up to `budget` for them to
    /// go, then SIGKILL what is left.
    pub(super) fn stop_tree(me: i32, budget: Duration) {
        let pids = of(me);
        if pids.is_empty() {
            return;
        }
        eprintln!("[e2e] stopping {} subprocess(es) of the interrupted setup", pids.len());
        signal_all(&pids, libc::SIGTERM);
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if of(me).is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let left = of(me);
        eprintln!("[e2e] {} subprocess(es) ignored SIGTERM; sending SIGKILL", left.len());
        signal_all(&left, libc::SIGKILL);
    }
}

#[cfg(not(target_os = "linux"))]
mod descendants {
    pub(super) fn stop(_budget: std::time::Duration) {}
}

#[cfg(unix)]
mod signal_record {
    use super::LAST_SIGNAL;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];
    static PREVIOUS: [AtomicUsize; 3] = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];

    extern "C" fn record(signal: libc::c_int) {
        LAST_SIGNAL.store(signal, Ordering::SeqCst);
        let Some(i) = SIGNALS.iter().position(|s| *s == signal) else {
            return;
        };
        let previous = PREVIOUS[i].load(Ordering::SeqCst);
        if previous != libc::SIG_DFL && previous != libc::SIG_IGN && previous != libc::SIG_ERR {
            // NOTE: `install` stores only handlers without SA_SIGINFO, so
            // `previous` has the one-argument handler ABI.
            let forward: extern "C" fn(libc::c_int) = unsafe { std::mem::transmute(previous) };
            forward(signal);
        }
    }

    /// Put `record` in front of the handler ctrlc installed for each
    /// signal. A signal whose current handler uses SA_SIGINFO is left
    /// alone; it then exits 130.
    pub(super) unsafe fn install() {
        for (i, signal) in SIGNALS.iter().enumerate() {
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(*signal, std::ptr::null(), &mut old) != 0 {
                    continue;
                }
                if old.sa_flags & libc::SA_SIGINFO != 0 {
                    continue;
                }
                PREVIOUS[i].store(old.sa_sigaction, Ordering::SeqCst);
                let mut new = old;
                new.sa_sigaction = record as extern "C" fn(libc::c_int) as usize;
                libc::sigaction(*signal, &new, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(not(unix))]
mod signal_record {
    pub(super) unsafe fn install() {}
}

/// Serialises every test, in this module or elsewhere in the crate, that
/// calls [`register`], [`drain`] or [`pending`] against the shared global
/// `STATE`. `cargo test` runs tests in parallel threads of one process, so
/// without this a `drain` in one test can take an entry a different test
/// registered a moment earlier.
#[cfg(test)]
pub(crate) fn test_serial() -> &'static Mutex<()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    &SERIAL
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_child;
    use std::io::BufRead;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use super::test_serial as serial;

    #[test]
    fn run_executes_once_and_drain_skips_it() {
        let _g = serial().lock().unwrap_or_else(|p| p.into_inner());
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let r = register("unit", move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        r.run();
        let report = drain(Duration::from_secs(1));
        assert!(report.started.is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn drop_deregisters_without_running() {
        let _g = serial().lock().unwrap_or_else(|p| p.into_inner());
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        drop(register("unit", move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        let report = drain(Duration::from_secs(1));
        assert!(report.started.is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn drain_runs_each_pending_teardown_once() {
        let _g = serial().lock().unwrap_or_else(|p| p.into_inner());
        let count = Arc::new(AtomicUsize::new(0));
        let regs: Vec<_> = (0..3)
            .map(|i| {
                let c = count.clone();
                register(format!("unit-{i}"), move || {
                    c.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        let report = drain(Duration::from_secs(5));
        assert_eq!(report.started.len(), 3);
        assert!(report.unfinished.is_empty());
        for r in regs {
            r.run();
        }
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn drain_reports_teardown_past_budget() {
        let _g = serial().lock().unwrap_or_else(|p| p.into_inner());
        let _r = register("slow", || std::thread::sleep(Duration::from_millis(800)));
        let report = drain(Duration::from_millis(100));
        assert_eq!(report.unfinished, vec!["slow".to_string()]);
        let report = drain(Duration::from_secs(5));
        assert!(report.unfinished.is_empty());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code_for(libc::SIGINT), 130);
        assert_eq!(exit_code_for(libc::SIGTERM), 143);
        assert_eq!(exit_code_for(0), 130);
    }

    const CHILD_ENV: &str = "FIRESTREAM_E2E_TEST_SIGNAL_CHILD";

    /// Child half of the signal tests. `CHILD_ENV` is `<mode>:<marker file>`.
    /// Mode `pending`: one registered teardown appends `ran` to the marker.
    /// Mode `unwinding`: a thread is inside `Registration::run` (appends
    /// `start`, sleeps, appends `done`) when the signal arrives.
    #[test]
    fn child_signal_target() {
        let Ok(spec) = std::env::var(CHILD_ENV) else {
            return;
        };
        let (mode, marker) = spec.split_once(':').unwrap();
        let marker = PathBuf::from(marker);
        install_signal_handler().unwrap();
        let append = |path: PathBuf, word: &'static str| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(f, "{word}").unwrap();
        };
        match mode {
            "pending" => {
                let m = marker.clone();
                let r = register("child pending", move || append(m, "ran"));
                println!("READY");
                std::thread::sleep(Duration::from_secs(30));
                r.disarm();
            }
            "unwinding" => {
                let m = marker.clone();
                let r = register("child unwinding", move || {
                    append(m.clone(), "start");
                    println!("READY");
                    std::thread::sleep(Duration::from_millis(1500));
                    append(m, "done");
                });
                std::thread::spawn(move || r.run());
                std::thread::sleep(Duration::from_secs(30));
            }
            other => panic!("unknown mode {other}"),
        }
    }

    fn run_signal_child(mode: &str, signal: libc::c_int) -> (i32, String) {
        let marker = std::env::temp_dir().join(format!(
            "fs-e2e-core-signal-{}-{}",
            std::process::id(),
            crate::run_id::RunId::random()
        ));
        let spec = format!("{mode}:{}", marker.display());
        let mut child = test_child::spawn("cleanup::tests::child_signal_target", &[(CHILD_ENV, &spec)]);
        let stdout = child.stdout.take().unwrap();
        let mut lines = std::io::BufReader::new(stdout).lines();
        loop {
            let line = lines.next().expect("child exited before READY").unwrap();
            if line.contains("READY") {
                break;
            }
        }
        assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, signal) }, 0);
        let status = child.wait().unwrap();
        let content = std::fs::read_to_string(&marker).unwrap_or_default();
        let _ = std::fs::remove_file(&marker);
        (status.code().expect("child exited by signal, not by handler"), content)
    }

    #[test]
    fn sigterm_drains_registry_and_exits_143() {
        let (code, marker) = run_signal_child("pending", libc::SIGTERM);
        assert_eq!(code, 143);
        assert_eq!(marker, "ran\n");
    }

    #[test]
    fn sigint_drains_registry_and_exits_130() {
        let (code, marker) = run_signal_child("pending", libc::SIGINT);
        assert_eq!(code, 130);
        assert_eq!(marker, "ran\n");
    }

    #[test]
    fn signal_during_guard_teardown_waits_and_runs_once() {
        let (code, marker) = run_signal_child("unwinding", libc::SIGINT);
        assert_eq!(code, 130);
        assert_eq!(marker, "start\ndone\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stop_tree_ends_grandchildren() {
        // NOTE: the root is a shell, not this process. Tests run in parallel
        // threads, and a stop on this process would kill their children too.
        let mut shell = std::process::Command::new("sh")
            .args(["-c", "sleep 60 & wait $!"])
            .spawn()
            .expect("spawn sh");
        let shell_pid = shell.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(5);
        let sleep = loop {
            if let Some(&p) = descendants::of(shell_pid).iter().next() {
                break p;
            }
            assert!(Instant::now() < deadline, "sleep grandchild never appeared");
            std::thread::sleep(Duration::from_millis(20));
        };

        descendants::stop_tree(shell_pid, Duration::from_secs(5));
        let status = shell.wait().expect("reap sh");
        assert!(descendants::of(shell_pid).is_empty(), "sleep {sleep} still alive");
        assert!(!status.success(), "sh must see its sleep killed: {status}");
    }
}
