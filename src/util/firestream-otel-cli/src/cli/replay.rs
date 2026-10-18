//! `otel-cli replay` — re-emit orphaned background spans from the checkpoint
//! log (PRD §9.3).
//!
//! This is the host-side counterpart to the `server json` startup hook: both
//! call [`crate::checkpoint::replay_dir`], so the flock + scan + emit + rename
//! routine is shared and the two paths can never double-emit (advisory lock +
//! `.jsonl.done` rename).
//!
//! `bin/ci/replay-spans.sh` wraps this subcommand on a 5-minute timer.

use std::time::Duration;

use anyhow::Result;
use clap::Args;

use super::common::CommonArgs;
use crate::config::{parse_attrs, Config};

#[derive(Debug, Args)]
pub struct ReplayArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Checkpoint directory to scan. Falls back to `OTEL_CHECKPOINT_DIR`.
    #[arg(long = "checkpoint-dir", env = "OTEL_CHECKPOINT_DIR")]
    pub checkpoint_dir: Option<String>,

    /// Only process run files at least this many seconds old. The host-side
    /// daemon uses 600 (10 min) so it never races a still-running CI server.
    #[arg(long = "min-age-seconds", default_value_t = 0)]
    pub min_age_seconds: u64,

    /// Directory to emit recovered spans into when `--protocol json+file`.
    /// Mostly for testing / the json sink; the host daemon emits over OTLP.
    #[arg(long = "json-dir")]
    pub json_dir: Option<String>,
}

#[cfg(unix)]
pub async fn run(args: ReplayArgs) -> Result<u8> {
    let cfg = build_config(&args)?;

    let dir = match cfg.resolved_checkpoint_dir() {
        Some(d) => d,
        None => {
            eprintln!("otel-cli replay: no checkpoint dir (set --checkpoint-dir or OTEL_CHECKPOINT_DIR); nothing to do");
            return Ok(0);
        }
    };

    let min_age = Duration::from_secs(args.min_age_seconds);
    let report = crate::checkpoint::replay_dir(&dir, &cfg, min_age).await?;
    eprintln!(
        "otel-cli replay: dir={} processed={} locked_out={} orphans_emitted={} rotated={}",
        dir.display(),
        report.files_processed,
        report.files_locked_out,
        report.orphans_emitted,
        report.files_rotated,
    );
    Ok(0)
}

#[cfg(not(unix))]
pub async fn run(_args: ReplayArgs) -> Result<u8> {
    anyhow::bail!("span replay is not supported on Windows");
}

/// Assemble config: defaults → env → CLI overlay. Replay needs the same
/// transport selection (`--endpoint`, `--tee`, `--protocol`, …) as live spans
/// so orphans land on the configured OTLP receiver.
fn build_config(args: &ReplayArgs) -> Result<Config> {
    let mut cfg = Config::defaults();
    cfg.load_env()?;

    if let Some(v) = &args.checkpoint_dir {
        cfg.checkpoint_dir = v.clone();
    }
    if let Some(v) = &args.json_dir {
        cfg.json_dir = v.clone();
    }
    if let Some(v) = &args.common.endpoint {
        cfg.endpoint = v.clone();
    }
    if let Some(v) = &args.common.traces_endpoint {
        cfg.traces_endpoint = v.clone();
    }
    if let Some(v) = &args.common.protocol {
        cfg.protocol = v.clone();
    }
    if let Some(v) = &args.common.timeout {
        cfg.timeout = v.clone();
    }
    if let Some(v) = &args.common.otlp_headers {
        cfg.headers = parse_attrs(v).map_err(|e| anyhow::anyhow!("--otlp-headers: {e}"))?;
    }
    if args.common.insecure {
        cfg.insecure = true;
    }
    if args.common.blocking {
        cfg.blocking = true;
    }
    if args.common.verbose {
        cfg.verbose = true;
    }
    if args.common.fail {
        cfg.fail = true;
    }
    if let Some(v) = &args.common.tee {
        cfg.tee = v.clone();
    }
    if let Some(v) = &args.common.tee_file_dir {
        cfg.tee_file_dir = v.clone();
    }
    if args.common.tee_durable {
        cfg.tee_durable = true;
    }

    Ok(cfg)
}
