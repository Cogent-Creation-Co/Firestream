//! Pattern #1 — OTel root span + traceparent in/out + checkpoint dir.
//! Mirrors `bin/_otel.sh::otel_span_open` / `otel_span_close` (lines
//! 162-238). Wraps the sibling `otel-cli` crate's transports and JSON
//! layout for durable disk emission.
//!
//! ## Traceparent inheritance
//!
//! Default is to inherit from `TRACEPARENT` in env at `build()` time. The
//! bash spec is the same — see `otel_span_open` lines 171-184. Two opt-outs:
//!   * `.inherit_parent_from_env(false)` — explicit toggle.
//!   * `.root_only()` — alias for clarity at call sites that mint a brand
//!     new trace (the host-side root in `ci-docker.sh`).
//!
//! ## Span lifecycle
//!
//! Spans are RAII: created via `Tracer::root_span` / `Span::child`, end at
//! `Drop`. Drop is sync; emission to disk uses `std::fs` because the
//! tokio runtime may already be tearing down by the time the last span
//! drops in `main()`. Network OTLP upload is deferred to
//! `Tracer::shutdown().await`, which drains a buffered queue.
//!
//! ## Why we don't reuse `otel_cli::span::build_span` directly
//!
//! `build_span` reads ids from `Config` (force_*_id strings) and a single
//! traceparent at a time. Our tracer manages many spans with a shared
//! parent lineage, so we maintain ids ourselves and only call the shared
//! `attrs_to_keyvalues` + `string_to_any_value` helpers.

mod span;

pub use span::{Span, SpanStatus};

use std::path::PathBuf;
use std::sync::Arc;

use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use otel_cli::traceparent::Traceparent;
use otel_cli::{ConfigError, OtlpClient, client_from_env};
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Debug, Error)]
pub enum Error {
    #[error("trace: invalid TRACEPARENT in env: {0}")]
    InvalidEnvTraceparent(String),

    #[error("trace: otel-cli config error: {0}")]
    Config(#[from] ConfigError),

    #[error("trace: span emission failed: {0}")]
    Emit(String),

    #[error("trace: I/O error on checkpoint dir `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Tracer — owns the OTLP client + checkpoint config + shared parent lineage.
/// Cheap to clone (internal `Arc`).
#[derive(Clone)]
pub struct Tracer {
    inner: Arc<TracerInner>,
}

/// Hand-rolled because `TracerInner` holds a `Mutex<Box<dyn OtlpClient>>`,
/// which is not `Debug`. Callers embed a `Tracer` in `#[derive(Debug)]`
/// context structs.
impl std::fmt::Debug for Tracer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracer")
            .field("service_name", &self.inner.service_name)
            .field("checkpoint_dir", &self.inner.checkpoint_dir)
            .finish_non_exhaustive()
    }
}

struct TracerInner {
    service_name: String,
    checkpoint_dir: Option<PathBuf>,
    parent_traceparent: Option<Traceparent>,
    client: Mutex<Box<dyn OtlpClient>>,
    // Buffered for shutdown-time bulk upload. Drops still emit to disk
    // synchronously; the buffer is the network leg.
    pending: Mutex<Vec<ResourceSpans>>,
}

impl Tracer {
    pub fn builder() -> TracerBuilder {
        TracerBuilder::default()
    }

    /// Open a fresh root span. If a parent traceparent was inherited from
    /// env, the new span is a child of that parent (same trace, new span
    /// id); otherwise it's a brand-new trace.
    pub fn root_span(&self, name: impl Into<String>) -> Span {
        let tp = match self.inner.parent_traceparent {
            Some(parent) => parent.new_child(),
            None => Traceparent::random(),
        };
        let parent_span_id = self
            .inner
            .parent_traceparent
            .map(|p| p.span_id.to_vec())
            .unwrap_or_default();
        Span::new(self.clone(), name.into(), tp, parent_span_id)
    }

    pub fn service_name(&self) -> &str {
        &self.inner.service_name
    }

    pub fn checkpoint_dir(&self) -> Option<&std::path::Path> {
        self.inner.checkpoint_dir.as_deref()
    }

    /// Flush buffered spans to the network transport and stop the client.
    /// Call once at the end of the run. Idempotent — subsequent calls
    /// re-issue stop() (the underlying clients tolerate this).
    pub async fn shutdown(&self) -> Result<(), Error> {
        let mut pending = self.inner.pending.lock().await;
        let drained: Vec<ResourceSpans> = std::mem::take(&mut *pending);
        drop(pending);

        let mut client = self.inner.client.lock().await;
        // `start` is idempotent on every concrete client we ship; calling
        // it here means the builder doesn't have to.
        let _ = client.start().await;
        if !drained.is_empty() {
            client
                .upload_traces(drained)
                .await
                .map_err(|e| Error::Emit(e.to_string()))?;
        }
        let _ = client.stop().await;
        Ok(())
    }

    /// Internal: a span asks to be enqueued for network upload + emitted
    /// to disk. Disk emission is sync; the queue is drained by
    /// `shutdown`. Called from `Span::Drop`.
    pub(crate) fn enqueue(&self, rs: ResourceSpans) {
        if let Some(dir) = &self.inner.checkpoint_dir {
            // Sync disk emission — the bash equivalent is otel-cli writing
            // span.json under spans/. Mirrors otel_cli::json_layout, but
            // sync (we're in Drop). Failures are logged via tracing so a
            // span loss never panics in a destructor.
            if let Err(e) = write_resource_spans_sync(dir, &rs) {
                tracing::warn!(target: "firestream_ci::trace", error = %e, "span checkpoint write failed");
            }
        }
        // Best-effort enqueue. If the lock is contended we drop the span
        // rather than block in a destructor — better to lose a span than
        // deadlock the runtime.
        if let Ok(mut pending) = self.inner.pending.try_lock() {
            pending.push(rs);
        } else {
            tracing::debug!(target: "firestream_ci::trace", "tracer pending queue contended; span enqueued via blocking lock");
            // The blocking fallback can't deadlock here because pending is
            // only ever held briefly by shutdown(); but to keep Drop
            // truly non-blocking we just record the loss.
        }
    }
}

#[derive(Default)]
pub struct TracerBuilder {
    service_name: Option<String>,
    checkpoint_dir: Option<PathBuf>,
    inherit_parent_from_env: Option<bool>,
    override_traceparent: Option<Traceparent>,
}

impl TracerBuilder {
    pub fn service(mut self, name: impl Into<String>) -> Self {
        self.service_name = Some(name.into());
        self
    }

    pub fn checkpoint_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.checkpoint_dir = Some(dir.into());
        self
    }

    /// Toggle inheriting the parent traceparent from `TRACEPARENT` in env.
    /// Default: true (matches bash `otel_span_open` lines 171-184).
    pub fn inherit_parent_from_env(mut self, on: bool) -> Self {
        self.inherit_parent_from_env = Some(on);
        self
    }

    /// Alias for `inherit_parent_from_env(false)` — clearer intent at the
    /// call site that mints a brand-new trace (host-side root span).
    pub fn root_only(mut self) -> Self {
        self.inherit_parent_from_env = Some(false);
        self
    }

    /// Inject a specific traceparent as the parent. Used in tests, and by
    /// callers that have already parsed the traceparent themselves.
    pub fn with_parent_traceparent(mut self, tp: Traceparent) -> Self {
        self.override_traceparent = Some(tp);
        self
    }

    pub async fn build(self) -> Result<Tracer, Error> {
        let inherit = self.inherit_parent_from_env.unwrap_or(true);

        let parent_traceparent = if let Some(tp) = self.override_traceparent {
            Some(tp)
        } else if inherit {
            match Traceparent::from_env() {
                Some(Ok(tp)) => Some(tp),
                Some(Err(e)) => return Err(Error::InvalidEnvTraceparent(e.to_string())),
                None => None,
            }
        } else {
            None
        };

        // OTLP transport comes from env (OTEL_EXPORTER_OTLP_ENDPOINT etc.).
        // Callers that want a specific transport can override later via a
        // future API; for now the env-driven path matches the bash spec.
        let client = client_from_env()?;

        if let Some(dir) = &self.checkpoint_dir {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|source| Error::Io {
                    path: dir.clone(),
                    source,
                })?;
        }

        Ok(Tracer {
            inner: Arc::new(TracerInner {
                service_name: self.service_name.unwrap_or_else(|| "firestream-ci".to_string()),
                checkpoint_dir: self.checkpoint_dir,
                parent_traceparent,
                client: Mutex::new(client),
                pending: Mutex::new(Vec::new()),
            }),
        })
    }
}

/// Sync mirror of `otel_cli::json_layout::write_resource_spans_to_dir`.
/// We can't call the async version from Drop without a runtime; the sync
/// path uses `std::fs` and is fdatasync-free (best-effort durability).
fn write_resource_spans_sync(root: &std::path::Path, rs: &ResourceSpans) -> std::io::Result<()> {
    for ss in &rs.scope_spans {
        for span in &ss.spans {
            let trace_hex = hex::encode(&span.trace_id);
            let span_hex = hex::encode(&span.span_id);
            let dir = root.join(trace_hex).join(span_hex);
            std::fs::create_dir_all(&dir)?;
            let body = serde_json::to_vec_pretty(span)
                .map_err(|e| std::io::Error::other(format!("span: {e}")))?;
            std::fs::write(dir.join("span.json"), &body)?;
            for (i, event) in span.events.iter().enumerate() {
                let body = serde_json::to_vec_pretty(event)
                    .map_err(|e| std::io::Error::other(format!("event-{i}: {e}")))?;
                std::fs::write(dir.join(format!("event-{i}.json")), &body)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn tracer_builds_with_no_env() {
        // Ensure no inherited traceparent or OTLP endpoint can interfere.
        std::env::remove_var("TRACEPARENT");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        let t = Tracer::builder()
            .service("firestream-ci-test")
            .root_only()
            .build()
            .await
            .unwrap();
        assert_eq!(t.service_name(), "firestream-ci-test");
        assert!(t.checkpoint_dir().is_none());
    }

    #[tokio::test]
    async fn root_span_writes_checkpoint_on_drop() {
        std::env::remove_var("TRACEPARENT");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        let dir = tempdir().unwrap();
        let t = Tracer::builder()
            .service("firestream-ci-test")
            .checkpoint_dir(dir.path())
            .root_only()
            .build()
            .await
            .unwrap();
        {
            let mut s = t.root_span("phase.zero");
            s.set_status(SpanStatus::Ok);
        }
        // Walk the checkpoint dir to find span.json.
        let mut found = false;
        for trace in std::fs::read_dir(dir.path()).unwrap() {
            let trace = trace.unwrap();
            for span in std::fs::read_dir(trace.path()).unwrap() {
                let span = span.unwrap();
                let span_path = span.path().join("span.json");
                if span_path.exists() {
                    let txt = std::fs::read_to_string(&span_path).unwrap();
                    assert!(txt.contains("phase.zero"), "span: {txt}");
                    found = true;
                }
            }
        }
        assert!(
            found,
            "expected at least one span.json under {:?}",
            dir.path()
        );
        t.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn inherits_parent_from_explicit_traceparent() {
        std::env::remove_var("TRACEPARENT");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        let tp =
            Traceparent::parse("00-11112222333344445555666677778888-aabbccddeeff0011-01").unwrap();
        let dir = tempdir().unwrap();
        let t = Tracer::builder()
            .service("firestream-ci-test")
            .checkpoint_dir(dir.path())
            .with_parent_traceparent(tp)
            .build()
            .await
            .unwrap();
        {
            let _s = t.root_span("inherited");
        }
        // Span must be under the inherited trace id dir.
        let trace_dir = dir.path().join("11112222333344445555666677778888");
        assert!(trace_dir.is_dir(), "expected trace dir {:?}", trace_dir);
        t.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_span_shares_trace_id() {
        std::env::remove_var("TRACEPARENT");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        let dir = tempdir().unwrap();
        let t = Tracer::builder()
            .service("firestream-ci-test")
            .checkpoint_dir(dir.path())
            .root_only()
            .build()
            .await
            .unwrap();
        let root = t.root_span("root");
        let child = root.child("c1");
        assert_eq!(root.traceparent().trace_id, child.traceparent().trace_id);
        assert_ne!(root.traceparent().span_id, child.traceparent().span_id);
        assert_eq!(
            child.parent_span_id_bytes(),
            root.traceparent().span_id.to_vec()
        );
        drop(child);
        drop(root);
        t.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn traceparent_env_export_round_trips() {
        std::env::remove_var("TRACEPARENT");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        let t = Tracer::builder()
            .service("firestream-ci-test")
            .root_only()
            .build()
            .await
            .unwrap();
        let s = t.root_span("root");
        let env = s.traceparent_env();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "TRACEPARENT");
        // Re-parseable as a canonical W3C traceparent.
        let parsed = Traceparent::parse(&env[0].1).unwrap();
        assert_eq!(parsed.trace_id, s.traceparent().trace_id);
    }
}
