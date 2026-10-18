//! In-process OpenTelemetry ingest. Replaces the per-build `otel-cli
//! nix-ingest` subprocess from the Python fork (`__init__.py:1131-1175`)
//! with a direct call into the `otel-cli` library.
//!
//! Lifecycle: `OtelIngest::new(opts)` is created once per `run()` and shared
//! across all build workers. Each build calls `fresh_state()` to get its own
//! `InProcessIngest` (so activity-id maps stay isolated, mirroring the
//! per-subprocess isolation the Python comment at `:126-128` requires), then
//! `feed_line` for each stderr line and `finish` on EOF.

use tokio::sync::Mutex;

use otel_cli::cli::nix_ingest::InProcessIngest;

use crate::options::Options;

/// Process-wide OTel ingest gating. Holds the configuration; each build
/// spawns its own `InProcessIngest` state machine via [`PerBuildIngest::fresh_state`].
pub struct PerBuildIngest {
    parent_trace: Option<String>,
    service: String,
    /// Default `--min-activity-level` used by `otel-cli nix-ingest` (4 = keep
    /// build + substitute, drop debug spam).
    min_level: i64,
    /// Serialize the construction step so multiple builds can't race on
    /// transport setup (the `client.start()` inside `InProcessIngest::new()`
    /// touches I/O; we don't want N builds banging on the same OTLP endpoint
    /// concurrently during the handshake). The actual feed_line / finish are
    /// per-build, lock-free.
    construction: Mutex<()>,
}

impl PerBuildIngest {
    pub fn from_options(opts: &Options) -> Option<Self> {
        if !opts.otel_ingest {
            return None;
        }
        Some(Self {
            parent_trace: opts.otel_parent_trace.clone(),
            service: opts.otel_service.clone(),
            min_level: 4,
            construction: Mutex::new(()),
        })
    }

    /// Build a fresh ingest state machine for one `nix build`. Returns `None`
    /// on transport-setup failure (matches the Python fork's behavior of
    /// continuing the build without spans rather than failing CI).
    pub fn fresh_state(&self) -> IngestState<'_> {
        IngestState::Pending {
            parent_trace: self.parent_trace.clone(),
            service: self.service.clone(),
            min_level: self.min_level,
            construction: &self.construction,
        }
    }

    /// Feed one line into `state`. Lazily constructs the underlying ingest
    /// on the first feed so we don't start an OTLP client for builds that
    /// emit zero internal-json lines (e.g. instant-cached builds).
    pub async fn feed_line(&self, state: &mut IngestState<'_>, line: &str) {
        if matches!(state, IngestState::Pending { .. }) {
            let IngestState::Pending {
                parent_trace,
                service,
                min_level,
                construction,
            } = std::mem::replace(state, IngestState::Disabled)
            else {
                unreachable!()
            };
            let _guard = construction.lock().await;
            match InProcessIngest::new(parent_trace, service, min_level).await {
                Ok(ingest) => {
                    *state = IngestState::Active(Box::new(ingest));
                }
                Err(e) => {
                    tracing::warn!("otel-cli ingest unavailable ({e}); building without spans");
                    *state = IngestState::Disabled;
                }
            }
        }
        if let IngestState::Active(ingest) = state {
            if let Err(e) = ingest.feed_line(line).await {
                tracing::warn!("otel ingest feed_line error: {e}");
            }
        }
    }

    /// Finish the per-build ingest. Honours the same 60-second timeout the
    /// Python fork applies (`__init__.py:1170-1175`): don't hang CI waiting
    /// on the ingest to drain.
    pub async fn finish(&self, state: IngestState<'_>) {
        let IngestState::Active(ingest) = state else {
            return;
        };
        let fut = ingest.finish();
        match tokio::time::timeout(std::time::Duration::from_secs(60), fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("otel ingest finish error: {e}"),
            Err(_) => tracing::warn!("otel ingest finish timed out after 60s"),
        }
    }
}

/// Per-build state. Construction is deferred until the first stderr line, so
/// we don't pay the transport-setup cost for builds that finish before
/// emitting any internal-json.
pub enum IngestState<'a> {
    Pending {
        parent_trace: Option<String>,
        service: String,
        min_level: i64,
        construction: &'a Mutex<()>,
    },
    Active(Box<InProcessIngest>),
    Disabled,
}
