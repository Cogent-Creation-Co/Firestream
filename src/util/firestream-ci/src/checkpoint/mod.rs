//! Pattern #18 from the plan: lock + scan + emit + rename loop — thin
//! wrapper over `otel_cli::checkpoint`. Backs the `firestream-ci spans replay`
//! subcommand and the leaf-script port (`bin/ci/replay-spans.sh`).
//!
//! The replay routine acquires a non-blocking `flock(2)` on each
//! `${run}.jsonl` file before processing and renames it to `.jsonl.done` on
//! success. This module wraps that machinery so the binary and library
//! callers share one entry point. The expensive work lives in
//! `otel_cli::checkpoint::replay_dir`; we only convert types.

use std::path::{Path, PathBuf};
use std::time::Duration;

use otel_cli::config::Config;
use thiserror::Error;

pub mod honeycomb;

pub use honeycomb::{
    HoneycombEnvInput, HoneycombEnvOutput, HoneycombReplayReport, compute_honeycomb_defaults,
    fill_honeycomb_defaults, replay_dir_to_honeycomb,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("checkpoint: replay failed: {0}")]
    Replay(String),

    #[error("checkpoint: otel-cli config: {0}")]
    Config(#[from] otel_cli::ConfigError),

    #[error("checkpoint: missing checkpoint dir (set --checkpoint-dir or OTEL_CHECKPOINT_DIR)")]
    MissingDir,

    #[error("checkpoint: replay unsupported on this platform")]
    Unsupported,
}

/// Aggregate counts returned by [`Replayer::run`].
#[derive(Debug, Clone, Default)]
pub struct ReplayReport {
    /// Files locked + processed + renamed to `.jsonl.done`.
    pub scanned: usize,
    /// Orphan spans emitted across all processed files.
    pub sent: usize,
    /// Files skipped because another processor held the lock.
    pub failed: usize,
    /// Files deleted by the dir-ceiling sweep.
    pub rotated: usize,
}

/// Replay orphaned checkpoint spans. Builder constructs an OTel client from
/// the ambient env (or one provided) and locks-and-replays each `.jsonl`
/// file under `checkpoint_dir`.
pub struct Replayer {
    checkpoint_dir: PathBuf,
    min_age: Duration,
    cfg: Config,
}

impl Replayer {
    pub fn builder() -> ReplayerBuilder {
        ReplayerBuilder::default()
    }

    /// Run one replay pass.
    #[cfg(unix)]
    pub async fn run(self) -> Result<ReplayReport, Error> {
        let inner = otel_cli::checkpoint::replay_dir(&self.checkpoint_dir, &self.cfg, self.min_age)
            .await
            .map_err(|e| Error::Replay(format!("{e:#}")))?;
        Ok(ReplayReport {
            scanned: inner.files_processed,
            sent: inner.orphans_emitted,
            failed: inner.files_locked_out,
            rotated: inner.files_rotated,
        })
    }

    #[cfg(not(unix))]
    pub async fn run(self) -> Result<ReplayReport, Error> {
        Err(Error::Unsupported)
    }

    pub fn checkpoint_dir(&self) -> &Path {
        &self.checkpoint_dir
    }
}

#[derive(Default)]
pub struct ReplayerBuilder {
    checkpoint_dir: Option<PathBuf>,
    min_age: Option<Duration>,
    cfg: Option<Config>,
    use_env: bool,
}

impl ReplayerBuilder {
    pub fn checkpoint_dir(mut self, p: impl Into<PathBuf>) -> Self {
        self.checkpoint_dir = Some(p.into());
        self
    }

    /// Only process run files at least this many seconds old. Default: 0
    /// (process everything).
    pub fn min_age(mut self, d: Duration) -> Self {
        self.min_age = Some(d);
        self
    }

    /// Supply an `otel-cli` config explicitly. If not set, the builder
    /// defaults to constructing one from the ambient env (`load_env()`).
    pub fn config(mut self, cfg: Config) -> Self {
        self.cfg = Some(cfg);
        self
    }

    /// Use the ambient env to populate the OTel client config. Default
    /// behaviour when no explicit `config` is set; calling this is a noop
    /// but makes intent explicit at the call site.
    pub fn from_env(mut self) -> Self {
        self.use_env = true;
        self
    }

    pub fn build(self) -> Result<Replayer, Error> {
        let dir = match self.checkpoint_dir {
            Some(d) => d,
            None => {
                // Fall back to env so the binary can stay clap-only.
                match std::env::var("OTEL_CHECKPOINT_DIR") {
                    Ok(s) if !s.is_empty() => PathBuf::from(s),
                    _ => return Err(Error::MissingDir),
                }
            }
        };

        let mut cfg = match self.cfg {
            Some(c) => c,
            None => Config::defaults(),
        };
        // Honeycomb wiring: if `HONEYCOMB_API_KEY` is set, fill blanks for
        // OTEL_EXPORTER_OTLP_* env vars *before* `load_env` reads them.
        // Never clobbers user-provided values. Offline (no key) is a no-op.
        let _ = honeycomb::fill_honeycomb_defaults();
        cfg.load_env()?;

        Ok(Replayer {
            checkpoint_dir: dir,
            min_age: self.min_age.unwrap_or(Duration::ZERO),
            cfg,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_dir_errors() {
        std::env::remove_var("OTEL_CHECKPOINT_DIR");
        let r = Replayer::builder().build();
        assert!(matches!(r, Err(Error::MissingDir)));
    }

    #[test]
    fn explicit_dir_builds() {
        let r = Replayer::builder()
            .checkpoint_dir("/tmp/firestream-ci-test")
            .min_age(Duration::from_secs(5))
            .build()
            .unwrap();
        assert_eq!(r.checkpoint_dir(), Path::new("/tmp/firestream-ci-test"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn empty_dir_yields_zero_counts() {
        let tmp = tempfile::tempdir().unwrap();
        // Don't let env leak into the json+file fallback path.
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        let r = Replayer::builder()
            .checkpoint_dir(tmp.path())
            .build()
            .unwrap()
            .run()
            .await
            .unwrap();
        assert_eq!(r.scanned, 0);
        assert_eq!(r.sent, 0);
        assert_eq!(r.failed, 0);
    }
}
