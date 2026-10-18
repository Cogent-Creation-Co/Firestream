//! `nix-eval-jobs` orchestration.

pub mod jobs;

pub use jobs::{Job, RawEvalLine};

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use crate::options::{EvalMode, Options, maybe_remote, nix_shell};

/// Spawn `nix-eval-jobs` against `opts`, with `tmp_dir/gcroots` as the gc-roots
/// directory. Returns the child process with stdout piped (line-delimited
/// JSON). Mirrors Python `nix_eval_jobs()` at `__init__.py:730-789`.
pub async fn spawn_nix_eval_jobs(tmp_dir: &Path, opts: &Options) -> Result<Child> {
    let gcroots = tmp_dir.join("gcroots");
    let mut args: Vec<String> = vec![
        "--gc-roots-dir".into(),
        gcroots.to_string_lossy().into_owned(),
        "--force-recurse".into(),
        "--max-memory-size".into(),
        opts.eval_max_memory_size.to_string(),
        "--workers".into(),
        opts.eval_workers.to_string(),
        // Suppress the dirty-tree warning — CI evaluates against the user's
        // worktree by design and the warning prints once per parallel task.
        "--option".into(),
        "warn-dirty".into(),
        "false".into(),
    ];
    args.extend(opts.options.iter().cloned());
    if opts.impure {
        args.push("--impure".into());
    }

    match opts.eval_mode {
        EvalMode::Flake => {
            args.push("--flake".into());
            args.push(if opts.flake_fragment.is_empty() {
                opts.flake_url.clone()
            } else {
                format!("{}#{}", opts.flake_url, opts.flake_fragment)
            });
            if let Some(sel) = &opts.select_expr {
                args.push("--select".into());
                args.push(sel.clone());
            }
            for (input, url) in &opts.override_inputs {
                args.push("--override-input".into());
                args.push(input.clone());
                args.push(url.clone());
            }
            if let Some(lock) = &opts.reference_lock_file {
                args.push("--reference-lock-file".into());
                args.push(lock.clone());
            }
        }
        EvalMode::Expr => {
            args.extend(opts.expr_args.iter().cloned());
            // nix-eval-jobs only accepts a single --select; compose -A
            // navigation with any user-supplied --select function.
            match (&opts.expr_attr, &opts.select_expr) {
                (attr, Some(sel)) if !attr.is_empty() => {
                    args.push("--select".into());
                    args.push(format!("root: ({sel}) (root.{attr})"));
                }
                (attr, None) if !attr.is_empty() => {
                    args.push("--select".into());
                    args.push(format!("root: root.{attr}"));
                }
                (_, Some(sel)) => {
                    args.push("--select".into());
                    args.push(sel.clone());
                }
                _ => {}
            }
            args.push(opts.expr_file.clone());
        }
    }

    if opts.skip_cached {
        args.push("--check-cache-status".into());
    }

    // Prefix with the binary itself (or nix-shell fallback when remote).
    let mut full: Vec<String> = if opts.remote.is_some() {
        let mut shell = nix_shell("nixpkgs#nix-eval-jobs", "nix-eval-jobs");
        shell.extend(args);
        shell
    } else {
        let mut head = opts.nix_eval_jobs_bin.clone();
        head.extend(args);
        head
    };
    full = maybe_remote(full, opts);

    tracing::info!(
        "run {}",
        shlex::try_join(full.iter().map(String::as_str)).unwrap_or_default()
    );

    // When a caller wants the eval-stage stderr captured (for a stderr_log
    // file, a live tail ring, or just to keep it from leaking onto a TUI's
    // terminal), pipe stderr so we can drain it in `run.rs`. Otherwise keep
    // the long-standing `inherit` default so CLI users see warnings/errors
    // directly.
    let want_capture = opts.stderr_log.is_some() || opts.ring_sink.is_some();
    let stderr_stdio = if want_capture {
        Stdio::piped()
    } else {
        Stdio::inherit()
    };
    let child = Command::new(&full[0])
        .args(&full[1..])
        .stdout(Stdio::piped())
        .stderr(stderr_stdio)
        .spawn()
        .with_context(|| format!("spawn {}", full[0]))?;
    Ok(child)
}

/// Recognize lines that are pure noise from running CI on a host where the
/// `/etc/nix/nix.conf` declares daemon-only settings or where the eval
/// cache is being read concurrently by sibling tasks. Dropping them at the
/// capture boundary keeps the dashboard's tail-N preview useful without
/// hiding real diagnostics.
///
/// Substring match (conservative) — `warning: unknown setting '...'` always
/// targets specific names, and `error (ignored): SQLite database ... is
/// busy` always carries that exact phrase.
pub fn is_noisy_eval_line(line: &str) -> bool {
    line.contains("warning: unknown setting 'allowed-users'")
        || line.contains("warning: unknown setting 'trusted-users'")
        || (line.contains("error (ignored): SQLite database") && line.contains("is busy"))
}
