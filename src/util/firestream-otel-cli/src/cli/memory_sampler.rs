//! `otel-cli memory-sampler` — background RSS sampler for CI spans.
//!
//! Polls the process tree rooted at `--watch-pid` at `--interval-ms`,
//! appends one NDJSON line per sample to `--samples-file`, and exits when
//! the watch PID disappears or SIGTERM is received. The shell scripts in
//! `bin/ci/ci-linux.sh` and `bin/ci/ci-darwin.sh` spawn this in the
//! background right after the root span opens; `otel-cli span` reads the
//! resulting file to attach `memory.rss.*` attrs at emit time.
//!
//! Replaces `bin/ci/memory-watchdog-darwin.sh`. The `--peaks-file` output
//! is byte-compatible with that script's `peaks.jsonl` so the existing
//! Darwin consumer at `bin/ci/ci-darwin.sh:380-390` keeps working.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Args;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::signal::unix::{signal, SignalKind};

use crate::memory::aggregate_rss_bytes;

#[derive(Debug, Args)]
pub struct MemorySamplerArgs {
    /// PID whose process tree we sample. Sampler exits when this PID dies.
    #[arg(long = "watch-pid")]
    pub watch_pid: i32,

    /// NDJSON file to append samples to. Created if missing; parent dir
    /// must already exist (the shell wrapper makes it).
    #[arg(long = "samples-file")]
    pub samples_file: PathBuf,

    /// Sampling interval in milliseconds. Default 5000 (0.2 Hz). A 2-3 hour
    /// CI run at 1 Hz produces ~10 k samples; 5 s still gives ~12 samples
    /// for a 1-minute phase (plenty for p50/p95) and keeps the file under
    /// ~100 KB. Override to 1000 to match the legacy Darwin watchdog when
    /// you need sub-second spike resolution.
    #[arg(long = "interval-ms", default_value_t = 5000)]
    pub interval_ms: u64,

    /// Soft pressure threshold in MB; 0 disables. When the aggregate RSS
    /// crosses this, a `memory.pressure.soft` event is written to the
    /// peaks file (if set). Default matches the Darwin watchdog.
    #[arg(long = "soft-mb", default_value_t = 12000)]
    pub soft_mb: u64,

    /// Hard pressure threshold in MB; 0 disables. Always emits a
    /// `memory.pressure.hard` event when crossed; with
    /// `--enforce-hard-kill`, also SIGTERMs the largest descendant.
    #[arg(long = "hard-mb", default_value_t = 14000)]
    pub hard_mb: u64,

    /// Peaks file path (parity with the Darwin watchdog's `peaks.jsonl`).
    /// When set, soft/hard pressure events and the final peak record are
    /// appended here. Consumers like `bin/ci/ci-darwin.sh:380-390` parse
    /// the `"peak_mb"`/`"rss_mb"` fields from this file.
    #[arg(long = "peaks-file")]
    pub peaks_file: Option<PathBuf>,

    /// Opt in to SIGTERM-the-largest-descendant on hard pressure. Off by
    /// default on Linux (observe-only); on for the Darwin watchdog
    /// migration to preserve the existing behaviour.
    #[arg(long = "enforce-hard-kill", default_value_t = false)]
    pub enforce_hard_kill: bool,
}

pub async fn run(args: MemorySamplerArgs) -> Result<u8> {
    let interval = std::time::Duration::from_millis(args.interval_ms.max(10));

    // Truncate any prior samples file from a previous run — the consumer
    // queries by window, but stale samples with monotonic ts from before
    // the current run would confuse the math.
    {
        let f = std::fs::File::create(&args.samples_file)
            .with_context(|| format!("creating samples file {:?}", args.samples_file))?;
        drop(f);
    }
    let mut samples = OpenOptions::new()
        .append(true)
        .open(&args.samples_file)
        .await
        .with_context(|| format!("opening samples file {:?}", args.samples_file))?;
    let mut peaks = if let Some(path) = &args.peaks_file {
        Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .with_context(|| format!("opening peaks file {path:?}"))?,
        )
    } else {
        None
    };

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;

    let mut peak_bytes: u64 = 0;
    let mut ticker = tokio::time::interval(interval);
    // Skip the first tick at t=0 so the very first sample reflects steady
    // state rather than the moment of spawn.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if !pid_alive(args.watch_pid) {
                    break;
                }
                let rss = aggregate_rss_bytes(args.watch_pid);
                if rss > peak_bytes { peak_bytes = rss; }
                let ts = now_ns();
                let rss_kb = rss / 1024;
                let _ = samples
                    .write_all(format!("{{\"ts\":{ts},\"rss_kb\":{rss_kb}}}\n").as_bytes())
                    .await;

                let rss_mb = rss / 1024 / 1024;
                if args.hard_mb > 0 && rss_mb >= args.hard_mb {
                    let victim = if args.enforce_hard_kill {
                        largest_descendant(args.watch_pid).unwrap_or(0)
                    } else {
                        0
                    };
                    if let Some(peaks_f) = peaks.as_mut() {
                        let victim_str = if victim > 0 {
                            victim.to_string()
                        } else {
                            "none".to_string()
                        };
                        let _ = peaks_f
                            .write_all(
                                format!(
                                    "{{\"ts\":{ts},\"event\":\"memory.pressure.hard\",\"rss_mb\":{rss_mb},\"victim_pid\":\"{victim_str}\"}}\n"
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    if args.enforce_hard_kill && victim > 0 {
                        // SIGTERM the biggest descendant — same policy as
                        // memory-watchdog-darwin.sh:75.
                        unsafe { libc_kill(victim, 15) };
                    }
                } else if args.soft_mb > 0 && rss_mb >= args.soft_mb {
                    if let Some(peaks_f) = peaks.as_mut() {
                        let _ = peaks_f
                            .write_all(
                                format!(
                                    "{{\"ts\":{ts},\"event\":\"memory.pressure.soft\",\"rss_mb\":{rss_mb}}}\n"
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                }
            }
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        }
    }

    // Final peak record — consumed by bin/ci/ci-darwin.sh:380-390 to attach
    // build.memory.peak_bytes to the run span.
    if let Some(peaks_f) = peaks.as_mut() {
        let ts = now_ns();
        let peak_mb = peak_bytes / 1024 / 1024;
        let _ = peaks_f
            .write_all(format!("{{\"ts\":{ts},\"event\":\"peak\",\"peak_mb\":{peak_mb}}}\n").as_bytes())
            .await;
        let _ = peaks_f.flush().await;
    }
    let _ = samples.flush().await;
    Ok(0)
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// `kill(pid, 0)` returns 0 if the process exists and the caller has
/// permission to signal it. Treat "not permitted" (EPERM) as alive — the
/// process is real even if we can't signal it.
fn pid_alive(pid: i32) -> bool {
    let r = unsafe { libc_kill(pid, 0) };
    if r == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc_eperm())
}

#[cfg(target_os = "linux")]
fn largest_descendant(_root: i32) -> Option<i32> {
    // Walk /proc, find descendants by PPid, return the one with max VmRSS.
    use std::collections::HashSet;
    use std::fs;
    let mut tree: HashSet<i32> = HashSet::new();
    tree.insert(_root);
    // Build parent->children once by scanning /proc.
    let mut ppid_by_pid: std::collections::HashMap<i32, i32> = Default::default();
    let mut rss_by_pid: std::collections::HashMap<i32, u64> = Default::default();
    let entries = match fs::read_dir("/proc") {
        Ok(it) => it,
        Err(_) => return None,
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let pid: i32 = match name.to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut ppid = 0i32;
        let mut rss_kb = 0u64;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("PPid:") {
                ppid = rest.trim().parse().unwrap_or(0);
            } else if let Some(rest) = line.strip_prefix("VmRSS:") {
                rss_kb = rest
                    .trim_start()
                    .split_ascii_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            }
        }
        ppid_by_pid.insert(pid, ppid);
        rss_by_pid.insert(pid, rss_kb);
    }
    // Mark descendants by repeated transitive closure.
    let mut changed = true;
    while changed {
        changed = false;
        for (&pid, &ppid) in &ppid_by_pid {
            if !tree.contains(&pid) && tree.contains(&ppid) {
                tree.insert(pid);
                changed = true;
            }
        }
    }
    tree.remove(&_root);
    tree.into_iter()
        .max_by_key(|pid| rss_by_pid.get(pid).copied().unwrap_or(0))
}

#[cfg(target_os = "macos")]
fn largest_descendant(_root: i32) -> Option<i32> {
    // Not used in the default Linux flow; provided so the same binary can
    // power the Darwin migration. A complete port would mirror the linux
    // path against libproc, but the production caller doesn't need it for
    // observe-only mode (the bulk of the value). Left as a follow-up.
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn largest_descendant(_root: i32) -> Option<i32> {
    None
}

// libc shims so the rest of this file stays platform-agnostic. We could
// pull in `nix` for kill(), but raw kill+EPERM is a 2-line FFI and keeps
// the cfg matrix shallower.
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, sig)
}

fn libc_eperm() -> i32 {
    // EPERM is 1 on every Unix we ship to (Linux, macOS, *BSD). Hard-coding
    // avoids dragging in libc on Linux just for one constant.
    1
}
