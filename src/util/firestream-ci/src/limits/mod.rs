//! Pattern #21 — `LimitedCommand`: cgroup-bounded execution (Linux) +
//! polling watchdog (Darwin), behind one unified API. (Formerly mirrored
//! the now-deleted `bin/ci/run-with-limits-linux.sh` and
//! `bin/ci/memory-watchdog-darwin.sh`.)
//!
//! ## Linux (systemd-run)
//!
//! Wraps the inner `exec::Command` in `systemd-run --user --scope` with
//! `MemoryHigh`, `MemoryMax`, `TasksMax`, `IOWeight`, and — the knob that
//! actually binds on this hardware — `IOWriteBandwidthMax` /
//! `IOReadBandwidthMax` (cgroup io.max). `IOWeight` is proportional-share
//! and a silent no-op on NVMe without bfq/io.cost (the common case); only
//! the absolute io.max bandwidth caps reliably throttle a runaway build.
//! Requires `systemd-run` on PATH AND a functional user-slice
//! (`systemctl --user is-active default.target`). If either is missing, we
//! run unwrapped and emit a `tracing::warn!` — the bash did the same (fall
//! through is preserved behaviour, not a silent loss).
//!
//! ## Darwin (watchdog task)
//!
//! macOS has no cgroup memory accounting; the bash watchdog polls the PID
//! tree at 1 Hz via `ps`, sums RSS, and TERMs the largest descendant when
//! the hard threshold is crossed. We replicate the loop with `tokio::spawn`
//! plus the `nix` crate for `kill`. Failures (a transient `ps` or `kill -0`
//! error) never abort the watchdog (matches `set -u` but not `set -e` in
//! the bash).
//!
//! ## Why a wrapper, not a `Command` extension
//!
//! `exec::Command` already knows how to spawn a process; `LimitedCommand`
//! injects either the `systemd-run` argv prefix (Linux) or the watchdog
//! task (Darwin). The wrapper composes — callers reach in via
//! `.into_inner()` to retrieve the unwrapped `Command` if they need to.

use thiserror::Error;
use tracing::warn;

use crate::exec::{Command, Error as ExecError, ExitReport};
use crate::trace::Span;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Exec(#[from] ExecError),
}

/// Per-process resource budget. None = unlimited. CPU is a hint only — on
/// Linux we set `TasksMax=infinity` (no fork bombing implied), CPU pinning
/// is left to the caller via `taskset`.
#[derive(Debug, Clone, Default)]
pub struct Limits {
    /// Hard ceiling. systemd-run `MemoryMax=`; OOM-kill on exceed.
    /// Darwin: hard watchdog threshold, SIGTERM-the-largest-descendant.
    pub mem_max_mb: Option<u64>,
    /// Soft ceiling. systemd-run `MemoryHigh=`; throttling pressure.
    /// Darwin: soft watchdog threshold, writes a sentinel for the
    /// orchestrator to cut `--max-jobs` on the next phase.
    pub mem_high_mb: Option<u64>,
    /// Reserved — bash sets `TasksMax=infinity`, so no use today, but
    /// captured here so the API doesn't churn when we wire it.
    pub cpu_cores: Option<u32>,
    /// systemd `IOWeight=`. Defaults to 200 in the bash; mirror. NOTE:
    /// proportional-share IO weighting needs bfq or an io.cost model —
    /// on none-scheduler NVMe this is a silent no-op. Prefer the io.max
    /// caps below for anything that must actually bind.
    pub io_weight: Option<u32>,
    /// Absolute write-bandwidth cap in MB/s: systemd
    /// `IOWriteBandwidthMax=<device> <n>M` (cgroup io.max). Linux only;
    /// no Darwin analogue.
    pub io_write_max_mb: Option<u64>,
    /// Absolute read-bandwidth cap in MB/s: systemd
    /// `IOReadBandwidthMax=<device> <n>M`. Linux only.
    pub io_read_max_mb: Option<u64>,
    /// Filesystem path identifying the block device the io.max caps apply
    /// to (systemd resolves a path to its backing device). Defaults to `/`.
    pub io_device: Option<String>,
    /// Hard CPU ceiling as a percentage of one core (systemd `CPUQuota=`;
    /// 600 = six cores). Unlike `cpu_cores`, this actually binds. Linux
    /// only; no Darwin analogue.
    pub cpu_quota_pct: Option<u32>,
}

impl Limits {
    pub fn with_mem_max_mb(mut self, mb: u64) -> Self {
        self.mem_max_mb = Some(mb);
        self
    }
    pub fn with_mem_high_mb(mut self, mb: u64) -> Self {
        self.mem_high_mb = Some(mb);
        self
    }
    /// Convenience: matches the bash heuristic — `MemoryHigh ≈ 0.9 × MemoryMax`.
    pub fn with_mem_max_and_default_high(mut self, mb: u64) -> Self {
        self.mem_max_mb = Some(mb);
        self.mem_high_mb = Some((mb as f64 * 0.9).floor() as u64);
        self
    }
    pub fn with_io_write_max_mb(mut self, mb: u64) -> Self {
        self.io_write_max_mb = Some(mb);
        self
    }
    pub fn with_io_read_max_mb(mut self, mb: u64) -> Self {
        self.io_read_max_mb = Some(mb);
        self
    }
    pub fn with_io_device(mut self, device: impl Into<String>) -> Self {
        self.io_device = Some(device.into());
        self
    }
    pub fn with_cpu_quota_pct(mut self, pct: u32) -> Self {
        self.cpu_quota_pct = Some(pct);
        self
    }
}

/// The `systemd-run --user --scope …` argv prefix that would enforce
/// `limits`, or `None` when cgroup enforcement is unavailable here (non-Linux,
/// no `systemd-run` on PATH, or no functional user slice).
///
/// Exists so a caller that does **not** own an `exec::Command` — notably
/// `firestream_ci::nix::FastBuild`, which hands an argv to
/// `firestream_nix_build` and never spawns the child itself — can still put
/// its children in a bounded cgroup. Same probe, same argv, same fall-through
/// warning as [`LimitedCommand`].
pub async fn systemd_scope_prefix(limits: &Limits) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        if linux::systemd_available().await {
            Some(linux::scope_prefix(limits))
        } else {
            warn!(
                target: "firestream_ci::limits",
                "systemd-run unavailable; nix builds will run unwrapped (cgroup caps skipped)"
            );
            None
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = limits;
        None
    }
}

/// A memory-and-concurrency budget for a parallel build phase.
///
/// Why this exists: the plan's own risk table calls out that a parallel
/// `FastBuild` on the documented devcontainer minimum (4 CPU / 8 GB) will OOM,
/// and requires the cgroup caps to land in the *same* phase as the parallel
/// build. A `MemoryMax` alone does not do it — N concurrent scopes each capped
/// at X bound the total at N·X, not X. So the budget derives BOTH numbers from
/// one pool: the per-child cap is `pool / jobs`, and `jobs` is reduced until
/// each child gets at least [`MIN_MB_PER_JOB`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildBudget {
    /// Physical RAM observed (or the fallback assumption).
    pub total_mb: u64,
    /// The share of `total_mb` this CI run may use in aggregate.
    pub pool_mb: u64,
    /// `MemoryMax` for ONE nix build child.
    pub per_job_mb: u64,
    /// Concurrent nix build children, after memory-driven reduction.
    pub max_jobs: usize,
    /// True when `max_jobs` was reduced below the requested value.
    pub jobs_reduced: bool,
}

/// Fraction of physical RAM a CI run may claim, in percent.
pub const POOL_PERCENT: u64 = 75;
/// Floor for one nix build child. Below this, nix itself thrashes.
pub const MIN_MB_PER_JOB: u64 = 2048;
/// Assumed RAM when `/proc/meminfo` (or `sysctl`) cannot be read. Matches the
/// devcontainer minimum documented in `CLAUDE.md` — i.e. deliberately
/// pessimistic.
pub const FALLBACK_TOTAL_MB: u64 = 8192;

impl BuildBudget {
    /// Derive a budget for `requested_jobs` concurrent builds from `total_mb`.
    ///
    /// Pure — [`BuildBudget::detect`] is the impure wrapper — so the arithmetic
    /// is unit-testable at the devcontainer minimum without an 8 GB host.
    pub fn derive(total_mb: u64, requested_jobs: usize) -> Self {
        let total_mb = total_mb.max(1);
        let pool_mb = (total_mb * POOL_PERCENT / 100).max(MIN_MB_PER_JOB.min(total_mb));
        let requested = requested_jobs.max(1);
        // How many children can each hold the floor?
        let affordable = usize::try_from(pool_mb / MIN_MB_PER_JOB).unwrap_or(usize::MAX);
        let max_jobs = requested.min(affordable.max(1));
        let per_job_mb = (pool_mb / max_jobs as u64).max(1);
        Self {
            total_mb,
            pool_mb,
            per_job_mb,
            max_jobs,
            jobs_reduced: max_jobs < requested,
        }
    }

    /// [`BuildBudget::derive`] against the live host's physical RAM.
    pub fn detect(requested_jobs: usize) -> Self {
        Self::derive(detect_total_mb().unwrap_or(FALLBACK_TOTAL_MB), requested_jobs)
    }

    /// The per-child [`Limits`]: `MemoryMax` = `per_job_mb`, `MemoryHigh` =
    /// 90% of it (throttle before the OOM killer), plus the absolute
    /// io.max write cap — the only IO knob that binds on none-scheduler NVMe.
    pub fn per_job_limits(&self, io_write_max_mb: Option<u64>) -> Limits {
        let mut l = Limits::default().with_mem_max_and_default_high(self.per_job_mb);
        if let Some(mb) = io_write_max_mb.filter(|m| *m > 0) {
            l = l.with_io_write_max_mb(mb).with_io_device("/");
        }
        l
    }
}

/// Physical RAM in MB, or `None` when it cannot be determined.
pub fn detect_total_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
                return Some(kb / 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        let bytes: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(bytes / (1024 * 1024))
    }
}

/// Wraps an `exec::Command` with platform-specific resource limits. The
/// wrapped command's `.run` / `.run_unsupervised` API is mirrored; the
/// returned `ExitReport` is the inner command's report (limits are
/// transparent at the API level).
pub struct LimitedCommand {
    inner: Command,
    limits: Limits,
}

impl LimitedCommand {
    /// Wrap an existing `Command` with the supplied limits.
    pub fn wrap(inner: Command, limits: Limits) -> Self {
        Self { inner, limits }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Unwrap, throwing away the limits. Mostly useful in tests.
    pub fn into_inner(self) -> Command {
        self.inner
    }

    /// Run under a parent span. On Linux, prepends `systemd-run` argv if
    /// feature-detection passes; otherwise falls through unmodified. On
    /// Darwin, spawns the watchdog task before running and aborts it
    /// after.
    pub async fn run(self, parent: &Span) -> Result<ExitReport, Error> {
        #[cfg(target_os = "linux")]
        {
            let cmd = if linux::systemd_available().await {
                linux::wrap_with_systemd_run(self.inner, &self.limits)
            } else {
                warn!(
                    target: "firestream_ci::limits",
                    "systemd-run unavailable; running unwrapped (limits skipped)"
                );
                self.inner
            };
            Ok(cmd.run(parent).await?)
        }
        #[cfg(target_os = "macos")]
        {
            let watchdog = darwin::Watchdog::spawn(self.limits);
            let report = self.inner.run(parent).await;
            watchdog.abort();
            Ok(report?)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            warn!(
                target: "firestream_ci::limits",
                "limits unimplemented on this target_os; running unwrapped"
            );
            Ok(self.inner.run(parent).await?)
        }
    }

    /// Run without a parent span. Same wrapping logic as `run`.
    pub async fn run_unsupervised(self) -> Result<ExitReport, Error> {
        #[cfg(target_os = "linux")]
        {
            let cmd = if linux::systemd_available().await {
                linux::wrap_with_systemd_run(self.inner, &self.limits)
            } else {
                warn!(
                    target: "firestream_ci::limits",
                    "systemd-run unavailable; running unwrapped (limits skipped)"
                );
                self.inner
            };
            Ok(cmd.run_unsupervised().await?)
        }
        #[cfg(target_os = "macos")]
        {
            let watchdog = darwin::Watchdog::spawn(self.limits);
            let report = self.inner.run_unsupervised().await;
            watchdog.abort();
            Ok(report?)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            warn!(
                target: "firestream_ci::limits",
                "limits unimplemented on this target_os; running unwrapped"
            );
            Ok(self.inner.run_unsupervised().await?)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::sync::OnceLock;

    /// Probe once per process: `systemd-run --version` AND
    /// `systemctl --user is-active default.target`. The bash does the same
    /// two checks at lines 34 and 39 of `run-with-limits-linux.sh`.
    pub(super) async fn systemd_available() -> bool {
        // Cheap memo — re-checking on every spawn is wasted IPC, and the
        // result can't meaningfully change inside one CI run.
        static MEMO: OnceLock<bool> = OnceLock::new();
        if let Some(v) = MEMO.get() {
            return *v;
        }

        // Check 1: `systemd-run` on PATH.
        let on_path = tokio::process::Command::new("systemd-run")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if !on_path {
            let _ = MEMO.set(false);
            return false;
        }

        // Check 2: user-slice functional.
        let user_slice = tokio::process::Command::new("systemctl")
            .args(["--user", "is-active", "default.target"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        let _ = MEMO.set(user_slice);
        user_slice
    }

    /// Replace the inner command's program/argv with a `systemd-run` invocation
    /// that wraps the original. Inner env/cwd/sinks are preserved — see the
    /// `Command::build_wrapped` helper.
    pub(super) fn wrap_with_systemd_run(inner: Command, limits: &Limits) -> Command {
        inner.with_program_prefix(scope_prefix(limits))
    }

    /// The `systemd-run … --` argv prefix on its own. Shared by
    /// [`wrap_with_systemd_run`] and the public
    /// [`super::systemd_scope_prefix`], so a caller that assembles its own
    /// argv gets byte-identical enforcement.
    pub(super) fn scope_prefix(limits: &Limits) -> Vec<String> {
        let mut prefix = vec![
            "systemd-run".to_string(),
            "--user".to_string(),
            "--scope".to_string(),
            "--collect".to_string(),
        ];
        if let Some(mb) = limits.mem_high_mb {
            prefix.push("-p".to_string());
            prefix.push(format!("MemoryHigh={mb}M"));
        }
        if let Some(mb) = limits.mem_max_mb {
            prefix.push("-p".to_string());
            prefix.push(format!("MemoryMax={mb}M"));
        }
        prefix.push("-p".to_string());
        prefix.push("TasksMax=infinity".to_string());
        if let Some(io) = limits.io_weight {
            prefix.push("-p".to_string());
            prefix.push(format!("IOWeight={io}"));
        }
        // io.max absolute bandwidth caps — the only IO knob that binds on
        // none-scheduler NVMe. systemd accepts a filesystem path and
        // resolves it to the backing block device.
        let device = limits.io_device.as_deref().unwrap_or("/");
        if let Some(mb) = limits.io_write_max_mb {
            prefix.push("-p".to_string());
            prefix.push(format!("IOWriteBandwidthMax={device} {mb}M"));
        }
        if let Some(mb) = limits.io_read_max_mb {
            prefix.push("-p".to_string());
            prefix.push(format!("IOReadBandwidthMax={device} {mb}M"));
        }
        if let Some(pct) = limits.cpu_quota_pct {
            prefix.push("-p".to_string());
            prefix.push(format!("CPUQuota={pct}%"));
        }
        prefix.push("--".to_string());
        prefix
    }
}

#[cfg(target_os = "macos")]
mod darwin {
    use super::*;
    use std::time::Duration;
    use tokio::task::JoinHandle;

    pub(super) struct Watchdog {
        handle: Option<JoinHandle<()>>,
    }

    impl Watchdog {
        /// Spawn a 1 Hz polling task that scans `ps` for the parent's
        /// descendant tree, sums RSS, and TERMs the largest if the hard
        /// threshold is crossed. Returns immediately. The bash equivalent
        /// runs as a separate script — we keep it in-process.
        pub(super) fn spawn(limits: Limits) -> Self {
            let hard = limits.mem_max_mb.unwrap_or(u64::MAX);
            let soft = limits.mem_high_mb.unwrap_or(u64::MAX);
            // Self-pid is the root we poll. We poll our own descendants
            // because LimitedCommand spawns the inner child as our own
            // direct descendant.
            let root_pid = std::process::id();
            let handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                // Edge-trigger the soft-pressure event: emit once when RSS
                // crosses into soft territory and rearm only after it drops
                // back below. The bash watchdog logged every 1 Hz tick over
                // the soft threshold (one peaks.jsonl line per second); here
                // we surface a single observable warn per crossing instead.
                let mut soft_active = false;
                loop {
                    interval.tick().await;
                    let rss_mb = poll_rss_tree_mb(root_pid);
                    if rss_mb >= hard {
                        // Already past soft; suppress a redundant soft warn on
                        // the way back down through the soft band.
                        soft_active = true;
                        if let Some(victim) = largest_descendant(root_pid) {
                            #[cfg(unix)]
                            {
                                let _ = nix::sys::signal::kill(
                                    nix::unistd::Pid::from_raw(victim as i32),
                                    nix::sys::signal::Signal::SIGTERM,
                                );
                            }
                            tracing::warn!(
                                target: "firestream_ci::limits",
                                rss_mb, victim,
                                "darwin watchdog: hard memory pressure — SIGTERM'd largest descendant"
                            );
                        }
                    } else if rss_mb >= soft {
                        if !soft_active {
                            soft_active = true;
                            tracing::warn!(
                                target: "firestream_ci::limits",
                                rss_mb, soft_mb = soft,
                                "darwin watchdog: soft memory pressure — build memory is tight"
                            );
                        }
                    } else {
                        soft_active = false;
                    }
                }
            });
            Self {
                handle: Some(handle),
            }
        }

        pub(super) fn abort(mut self) {
            if let Some(h) = self.handle.take() {
                h.abort();
            }
        }
    }

    fn poll_rss_tree_mb(root: u32) -> u64 {
        let output = match std::process::Command::new("ps")
            .args(["-eo", "pid=,ppid=,rss="])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return 0,
        };
        let body = String::from_utf8_lossy(&output.stdout);
        let mut descendants: std::collections::HashSet<u32> = std::collections::HashSet::new();
        descendants.insert(root);

        // Build child map, then BFS from root.
        let mut parent_of: std::collections::HashMap<u32, u32> = Default::default();
        let mut rss_of: std::collections::HashMap<u32, u64> = Default::default();
        for line in body.lines() {
            let mut it = line.split_ascii_whitespace();
            if let (Some(pid), Some(ppid), Some(rss)) = (it.next(), it.next(), it.next()) {
                if let (Ok(pid), Ok(ppid), Ok(rss)) =
                    (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
                {
                    parent_of.insert(pid, ppid);
                    rss_of.insert(pid, rss);
                }
            }
        }
        // BFS the descendants closure.
        let mut frontier = vec![root];
        while let Some(p) = frontier.pop() {
            for (child, parent) in &parent_of {
                if *parent == p && !descendants.contains(child) {
                    descendants.insert(*child);
                    frontier.push(*child);
                }
            }
        }
        let total_kb: u64 = descendants
            .iter()
            .map(|p| rss_of.get(p).copied().unwrap_or(0))
            .sum();
        total_kb / 1024
    }

    fn largest_descendant(root: u32) -> Option<u32> {
        let output = std::process::Command::new("ps")
            .args(["-eo", "pid=,ppid=,rss="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let body = String::from_utf8_lossy(&output.stdout);
        let mut max: Option<(u32, u64)> = None;
        let mut parent_of: std::collections::HashMap<u32, u32> = Default::default();
        let mut rss_of: std::collections::HashMap<u32, u64> = Default::default();
        for line in body.lines() {
            let mut it = line.split_ascii_whitespace();
            if let (Some(pid), Some(ppid), Some(rss)) = (it.next(), it.next(), it.next()) {
                if let (Ok(pid), Ok(ppid), Ok(rss)) =
                    (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
                {
                    parent_of.insert(pid, ppid);
                    rss_of.insert(pid, rss);
                }
            }
        }
        let mut descendants: std::collections::HashSet<u32> = Default::default();
        let mut frontier = vec![root];
        while let Some(p) = frontier.pop() {
            for (child, parent) in &parent_of {
                if *parent == p && !descendants.contains(child) && *child != root {
                    descendants.insert(*child);
                    frontier.push(*child);
                }
            }
        }
        for d in &descendants {
            let rss = rss_of.get(d).copied().unwrap_or(0);
            match max {
                Some((_, best)) if rss <= best => {}
                _ => max = Some((*d, rss)),
            }
        }
        max.map(|(pid, _)| pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::Tracer;

    async fn quiet_tracer() -> Tracer {
        std::env::remove_var("TRACEPARENT");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        Tracer::builder()
            .service("firestream-ci-test")
            .root_only()
            .build()
            .await
            .unwrap()
    }

    #[test]
    fn limits_with_default_high_is_90_percent() {
        let l = Limits::default().with_mem_max_and_default_high(1000);
        assert_eq!(l.mem_max_mb, Some(1000));
        assert_eq!(l.mem_high_mb, Some(900));
    }

    #[test]
    fn limits_builder_individual_fields() {
        let l = Limits::default()
            .with_mem_max_mb(2048)
            .with_mem_high_mb(1800);
        assert_eq!(l.mem_max_mb, Some(2048));
        assert_eq!(l.mem_high_mb, Some(1800));
    }

    #[tokio::test]
    async fn run_returns_exit_report_passthrough() {
        // Even with no systemd / no Darwin watchdog, the unwrapped fallthrough
        // must still produce a typed report. This test gates the API contract.
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let limited =
            LimitedCommand::wrap(Command::new("sh").args(["-c", "exit 0"]), Limits::default());
        let report = limited.run(&parent).await.unwrap();
        assert!(report.success());
    }

    #[tokio::test]
    async fn limited_preserves_exit_code_on_failure() {
        let t = quiet_tracer().await;
        let parent = t.root_span("root");
        let limited = LimitedCommand::wrap(
            Command::new("sh").args(["-c", "exit 7"]),
            Limits::default().with_mem_max_and_default_high(256),
        );
        let report = limited.run(&parent).await.unwrap();
        assert_eq!(report.exit_code, 7);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn systemd_probe_does_not_panic() {
        // Whatever the host reports, the probe must complete. No `unwrap`s.
        let _ = linux::systemd_available().await;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_wrap_inserts_expected_args() {
        let inner = Command::new("echo").arg("hi");
        let wrapped = linux::wrap_with_systemd_run(
            inner,
            &Limits::default().with_mem_max_and_default_high(4096),
        );
        let argv = wrapped.argv_for_debug();
        assert_eq!(argv[0], "systemd-run");
        assert!(argv.contains(&"MemoryMax=4096M".to_string()));
        assert!(argv.contains(&"MemoryHigh=3686M".to_string()));
        assert!(argv.contains(&"TasksMax=infinity".to_string()));
        // Original argv after `--`
        let dd_pos = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[dd_pos + 1], "echo");
        assert_eq!(argv[dd_pos + 2], "hi");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_wrap_inserts_io_bandwidth_caps() {
        let inner = Command::new("echo").arg("hi");
        let wrapped = linux::wrap_with_systemd_run(
            inner,
            &Limits::default()
                .with_io_write_max_mb(400)
                .with_io_read_max_mb(1500),
        );
        let argv = wrapped.argv_for_debug();
        assert!(argv.contains(&"IOWriteBandwidthMax=/ 400M".to_string()));
        assert!(argv.contains(&"IOReadBandwidthMax=/ 1500M".to_string()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_wrap_io_caps_honor_custom_device() {
        let inner = Command::new("echo").arg("hi");
        let wrapped = linux::wrap_with_systemd_run(
            inner,
            &Limits::default()
                .with_io_write_max_mb(50)
                .with_io_device("/dev/nvme0n1"),
        );
        let argv = wrapped.argv_for_debug();
        assert!(argv.contains(&"IOWriteBandwidthMax=/dev/nvme0n1 50M".to_string()));
        // Read cap unset — must not be emitted.
        assert!(!argv.iter().any(|a| a.starts_with("IOReadBandwidthMax")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_wrap_inserts_cpu_quota() {
        let inner = Command::new("echo").arg("hi");
        let wrapped =
            linux::wrap_with_systemd_run(inner, &Limits::default().with_cpu_quota_pct(600));
        let argv = wrapped.argv_for_debug();
        assert!(argv.contains(&"CPUQuota=600%".to_string()));
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    /// The plan's stated failure mode: "Parallel FastBuild on the documented
    /// devcontainer minimum (4 CPU / 8 GB) will OOM." The budget must NOT hand
    /// out four 6 GB scopes on an 8 GB box.
    #[test]
    fn devcontainer_minimum_bounds_the_aggregate() {
        let b = BuildBudget::derive(8192, 4);
        assert_eq!(b.pool_mb, 6144, "75% of 8 GB");
        assert!(b.jobs_reduced, "4 jobs × 2 GB floor exceeds the 6 GB pool");
        assert_eq!(b.max_jobs, 3);
        assert_eq!(b.per_job_mb, 2048);
        // The invariant that matters: N scopes × MemoryMax stays inside the pool.
        assert!(b.per_job_mb * b.max_jobs as u64 <= b.pool_mb);
    }

    #[test]
    fn roomy_host_keeps_the_requested_concurrency() {
        let b = BuildBudget::derive(64 * 1024, 4);
        assert!(!b.jobs_reduced);
        assert_eq!(b.max_jobs, 4);
        assert_eq!(b.pool_mb, 49152);
        assert_eq!(b.per_job_mb, 12288);
        assert!(b.per_job_mb * b.max_jobs as u64 <= b.pool_mb);
    }

    /// A host too small for even one floor-sized job still yields exactly one
    /// job — never zero, which would deadlock the queue.
    #[test]
    fn tiny_host_still_gets_one_job() {
        let b = BuildBudget::derive(1024, 8);
        assert_eq!(b.max_jobs, 1);
        assert!(b.per_job_mb >= 1);
    }

    #[test]
    fn per_job_limits_carry_memory_and_io_caps() {
        let b = BuildBudget::derive(8192, 4);
        let l = b.per_job_limits(Some(512));
        assert_eq!(l.mem_max_mb, Some(2048));
        assert_eq!(l.mem_high_mb, Some(1843)); // floor(0.9 × 2048)
        assert_eq!(l.io_write_max_mb, Some(512));
        assert_eq!(l.io_device.as_deref(), Some("/"));
        // 0 / None means "leave IO alone" rather than "cap at zero".
        assert_eq!(b.per_job_limits(Some(0)).io_write_max_mb, None);
        assert_eq!(b.per_job_limits(None).io_write_max_mb, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scope_prefix_matches_the_command_wrapper() {
        let l = BuildBudget::derive(8192, 4).per_job_limits(Some(512));
        let prefix = linux::scope_prefix(&l);
        assert_eq!(prefix.first().unwrap(), "systemd-run");
        assert_eq!(prefix.last().unwrap(), "--");
        assert!(prefix.contains(&"MemoryMax=2048M".to_string()));
        assert!(prefix.contains(&"IOWriteBandwidthMax=/ 512M".to_string()));
        // Byte-identical to what LimitedCommand would have produced.
        let wrapped = linux::wrap_with_systemd_run(Command::new("echo"), &l);
        let argv = wrapped.argv_for_debug();
        assert_eq!(&argv[..prefix.len()], &prefix[..]);
    }
}
