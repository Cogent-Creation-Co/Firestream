//! Honeycomb env-injection + span replay.
//!
//! Phase 6 of the `_build/` production-ready refactor wires Honeycomb in
//! at *replay* time only. The local file sink (`json+file`) ignores OTLP
//! env vars; only `spans replay` (and the auto-replay hook at the end of
//! a successful CI run) ships bytes over the wire.
//!
//! This module is split into two halves:
//!
//! 1. A *pure* helper, [`compute_honeycomb_defaults`], that takes a
//!    snapshot of the relevant env vars and returns what
//!    `OTEL_EXPORTER_OTLP_PROTOCOL` / `_ENDPOINT` / `_HEADERS` should be
//!    set to *if not already set by the user*. This is unit-testable
//!    without touching the process env.
//! 2. An *impure* wrapper, [`fill_honeycomb_defaults`], that reads the
//!    real env, calls the pure helper, and writes the deltas back via
//!    `std::env::set_var`. Called from [`replay_dir_to_honeycomb`] (and
//!    indirectly from `Replayer::build()` via the same path).
//!
//! Anti-clobber rule: when `OTEL_EXPORTER_OTLP_HEADERS` already contains
//! an `x-honeycomb-team=` entry (case-sensitive match on the key — the
//! OTLP env-var spec is case-sensitive), the helper is a *no-op for that
//! header*. User configuration always wins. Same rule for
//! `x-honeycomb-dataset=`.

use std::path::Path;

use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use otel_cli::config::Config;
use tracing::warn;

use super::Error;

/// Snapshot of the four env vars the helper cares about. Constructed by
/// [`fill_honeycomb_defaults`] from the process env; tests construct it
/// directly so they can exercise every branch without `set_var`.
#[derive(Debug, Clone, Default)]
pub struct HoneycombEnvInput {
    pub api_key: Option<String>,
    pub dataset: Option<String>,
    pub existing_protocol: Option<String>,
    pub existing_endpoint: Option<String>,
    pub existing_headers: Option<String>,
}

/// What the helper *would* set, given the input. `None` means leave the
/// env var alone. The caller applies these via `set_var`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoneycombEnvOutput {
    pub protocol: Option<String>,
    pub endpoint: Option<String>,
    pub headers: Option<String>,
}

/// HTTP/protobuf is the default — it survives more proxies than gRPC
/// and `otel-cli` already links the HTTP client.
const DEFAULT_PROTOCOL: &str = "http/protobuf";
const HTTP_ENDPOINT: &str = "https://api.honeycomb.io/v1/traces";
const GRPC_ENDPOINT: &str = "https://api.honeycomb.io:443";

/// Pure: compute the env deltas needed to make `otel-cli` ship to
/// Honeycomb. Returns `Default::default()` when no API key is set; that
/// is the offline path and it MUST be byte-identical to today's
/// behaviour (no env writes).
pub fn compute_honeycomb_defaults(input: HoneycombEnvInput) -> HoneycombEnvOutput {
    let Some(api_key) = input.api_key.as_deref().filter(|s| !s.is_empty()) else {
        return HoneycombEnvOutput::default();
    };

    let mut out = HoneycombEnvOutput::default();

    // (1) Protocol — never overwrite user setting.
    let protocol_for_endpoint: String = match input.existing_protocol.as_deref() {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            out.protocol = Some(DEFAULT_PROTOCOL.to_string());
            DEFAULT_PROTOCOL.to_string()
        }
    };

    // (2) Endpoint — only when missing. The path component is required
    // for HTTP/protobuf (Honeycomb's HTTP exporter rejects bare host);
    // the gRPC endpoint is the host:port pair with no path.
    let needs_endpoint = input
        .existing_endpoint
        .as_deref()
        .filter(|s| !s.is_empty())
        .is_none();
    if needs_endpoint {
        match protocol_for_endpoint.as_str() {
            "http/protobuf" | "http/json" => {
                out.endpoint = Some(HTTP_ENDPOINT.to_string());
            }
            "grpc" => {
                out.endpoint = Some(GRPC_ENDPOINT.to_string());
            }
            other => {
                // Unknown protocol — refuse to guess. The replay will
                // either fail (no endpoint) or hit a user-provided one
                // we don't recognise; either way the operator is in
                // control. One warn line so it's not silent.
                warn!(
                    target: "firestream_ci::checkpoint::honeycomb",
                    protocol = other,
                    "honeycomb: unknown OTEL_EXPORTER_OTLP_PROTOCOL; leaving endpoint unset"
                );
            }
        }
    }

    // (3) + (4) Headers. The OpenTelemetry env-var spec uses
    // comma-separated `key=value` pairs. Anti-clobber: if a header key
    // is already present we leave it alone — user config wins.
    let existing_headers = input
        .existing_headers
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let team_present = existing_headers
        .as_deref()
        .map(|h| header_contains_key(h, "x-honeycomb-team"))
        .unwrap_or(false);
    let dataset_value = input.dataset.as_deref().filter(|s| !s.is_empty());
    let dataset_present = existing_headers
        .as_deref()
        .map(|h| header_contains_key(h, "x-honeycomb-dataset"))
        .unwrap_or(false);

    let mut to_append: Vec<String> = Vec::new();
    if !team_present {
        to_append.push(format!("x-honeycomb-team={api_key}"));
    }
    if let Some(ds) = dataset_value {
        if !dataset_present {
            to_append.push(format!("x-honeycomb-dataset={ds}"));
        }
    }

    if !to_append.is_empty() {
        let merged = match existing_headers {
            Some(h) => format!("{h},{}", to_append.join(",")),
            None => to_append.join(","),
        };
        out.headers = Some(merged);
    }

    out
}

/// Tests whether `headers` (comma-separated `k=v` pairs per the OTLP
/// env-var spec) already contains an entry whose key is exactly `key`.
/// Case-sensitive — the spec is case-sensitive.
fn header_contains_key(headers: &str, key: &str) -> bool {
    headers.split(',').any(|pair| {
        let pair = pair.trim();
        if let Some((k, _)) = pair.split_once('=') {
            k.trim() == key
        } else {
            false
        }
    })
}

/// Impure: read the process env, compute deltas, write them back. Called
/// before [`Config::load_env`] in the replay codepath so the `otel-cli`
/// config picks up the bootstrap values.
///
/// Returns the set of (var_name, new_value) pairs that were actually
/// applied. Empty when `HONEYCOMB_API_KEY` is unset — the offline path
/// stays byte-identical.
pub fn fill_honeycomb_defaults() -> Vec<(&'static str, String)> {
    let input = HoneycombEnvInput {
        api_key: std::env::var("HONEYCOMB_API_KEY").ok(),
        dataset: std::env::var("HONEYCOMB_DATASET").ok(),
        existing_protocol: std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").ok(),
        existing_endpoint: std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok(),
        existing_headers: std::env::var("OTEL_EXPORTER_OTLP_HEADERS").ok(),
    };
    let out = compute_honeycomb_defaults(input);
    let mut applied: Vec<(&'static str, String)> = Vec::new();
    if let Some(v) = out.protocol {
        std::env::set_var("OTEL_EXPORTER_OTLP_PROTOCOL", &v);
        applied.push(("OTEL_EXPORTER_OTLP_PROTOCOL", v));
    }
    if let Some(v) = out.endpoint {
        std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &v);
        applied.push(("OTEL_EXPORTER_OTLP_ENDPOINT", v));
    }
    if let Some(v) = out.headers {
        std::env::set_var("OTEL_EXPORTER_OTLP_HEADERS", &v);
        applied.push(("OTEL_EXPORTER_OTLP_HEADERS", v));
    }
    applied
}

/// Summary returned by [`replay_dir_to_honeycomb`].
#[derive(Debug, Clone, Default)]
pub struct HoneycombReplayReport {
    /// Spans successfully shipped via the OTLP client.
    pub shipped: usize,
    /// Spans that failed to parse or send. Non-fatal — logged + counted.
    pub failed: usize,
}

/// Walk `<rundir>/spans/<traceHex>/<spanHex>/span.json` and ship each
/// span as a `ResourceSpans` via the env-configured OTLP client.
///
/// Best-effort: parse failures and per-span send errors are logged via
/// `tracing::warn!` and tallied into `failed`; they never abort the
/// walk. The single log line at the end of the run looks like:
///
/// ```text
/// honeycomb: shipped N spans (failed M)
/// ```
///
/// We do *not* reuse `report::from_span_dir` here: that walker yields
/// the lossy `SpanEntry` summary type, which strips events, attributes,
/// links, and resource info — everything Honeycomb actually uses for
/// trace assembly. The replay path needs the raw `Span` proto. The walk
/// itself is a near-duplicate of the one in `cli::reconcile`, but it
/// returns parsed protos rather than rewriting them.
pub async fn replay_dir_to_honeycomb(spans_dir: &Path) -> Result<HoneycombReplayReport, Error> {
    let _ = fill_honeycomb_defaults();

    let mut cfg = Config::defaults();
    cfg.load_env()?;
    let mut client = otel_cli::client::build_client(&cfg);
    client
        .start()
        .await
        .map_err(|e| Error::Replay(format!("client start: {e}")))?;

    let mut report = HoneycombReplayReport::default();
    if !spans_dir.is_dir() {
        client
            .stop()
            .await
            .map_err(|e| Error::Replay(format!("client stop: {e}")))?;
        return Ok(report);
    }

    for entry in walkdir::WalkDir::new(spans_dir)
        .max_depth(4)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "span.json" {
            continue;
        }
        let path = entry.path();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    target: "firestream_ci::checkpoint::honeycomb",
                    path = %path.display(),
                    error = %e,
                    "honeycomb: span.json read failed"
                );
                report.failed += 1;
                continue;
            }
        };
        let span: Span = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    target: "firestream_ci::checkpoint::honeycomb",
                    path = %path.display(),
                    error = %e,
                    "honeycomb: span.json parse failed"
                );
                report.failed += 1;
                continue;
            }
        };
        let rs = ResourceSpans {
            resource: None,
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans: vec![span],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        };
        match client.upload_traces(vec![rs]).await {
            Ok(()) => report.shipped += 1,
            Err(e) => {
                warn!(
                    target: "firestream_ci::checkpoint::honeycomb",
                    path = %path.display(),
                    error = %e,
                    "honeycomb: upload_traces failed"
                );
                report.failed += 1;
            }
        }
    }

    client
        .stop()
        .await
        .map_err(|e| Error::Replay(format!("client stop: {e}")))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_with_api_key() -> HoneycombEnvInput {
        HoneycombEnvInput {
            api_key: Some("KEY".into()),
            ..Default::default()
        }
    }

    #[test]
    fn honeycomb_defaults_no_op_when_api_key_unset() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput::default());
        assert_eq!(out, HoneycombEnvOutput::default());
    }

    #[test]
    fn honeycomb_defaults_no_op_when_api_key_empty() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some(String::new()),
            ..Default::default()
        });
        assert_eq!(out, HoneycombEnvOutput::default());
    }

    #[test]
    fn honeycomb_defaults_set_endpoint_and_header_when_key_present() {
        let out = compute_honeycomb_defaults(input_with_api_key());
        assert_eq!(out.protocol.as_deref(), Some("http/protobuf"));
        assert_eq!(
            out.endpoint.as_deref(),
            Some("https://api.honeycomb.io/v1/traces"),
        );
        assert_eq!(out.headers.as_deref(), Some("x-honeycomb-team=KEY"));
    }

    #[test]
    fn honeycomb_defaults_respect_user_provided_endpoint() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some("KEY".into()),
            existing_endpoint: Some("https://my.collector/v1/traces".into()),
            ..Default::default()
        });
        // Protocol still defaults (user didn't set one), but endpoint
        // must not be overwritten.
        assert_eq!(out.protocol.as_deref(), Some("http/protobuf"));
        assert_eq!(out.endpoint, None);
    }

    #[test]
    fn honeycomb_defaults_respect_user_provided_protocol_grpc() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some("KEY".into()),
            existing_protocol: Some("grpc".into()),
            ..Default::default()
        });
        assert_eq!(out.protocol, None);
        assert_eq!(
            out.endpoint.as_deref(),
            Some("https://api.honeycomb.io:443"),
        );
    }

    #[test]
    fn honeycomb_defaults_merge_into_existing_headers_without_clobber() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some("KEY".into()),
            existing_headers: Some("foo=bar".into()),
            ..Default::default()
        });
        assert_eq!(out.headers.as_deref(), Some("foo=bar,x-honeycomb-team=KEY"),);
    }

    #[test]
    fn honeycomb_defaults_skip_team_header_when_already_present() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some("KEY".into()),
            existing_headers: Some("x-honeycomb-team=USERKEY,foo=bar".into()),
            ..Default::default()
        });
        // User-provided header wins — no append.
        assert_eq!(out.headers, None);
    }

    #[test]
    fn honeycomb_defaults_inject_dataset_when_set() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some("KEY".into()),
            dataset: Some("firestream-ci".into()),
            ..Default::default()
        });
        assert_eq!(
            out.headers.as_deref(),
            Some("x-honeycomb-team=KEY,x-honeycomb-dataset=firestream-ci"),
        );
    }

    #[test]
    fn honeycomb_defaults_skip_dataset_when_already_present() {
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some("KEY".into()),
            dataset: Some("firestream-ci".into()),
            existing_headers: Some("x-honeycomb-dataset=other".into()),
            ..Default::default()
        });
        // Team is still appended (not present); dataset is not.
        assert_eq!(
            out.headers.as_deref(),
            Some("x-honeycomb-dataset=other,x-honeycomb-team=KEY"),
        );
    }

    #[test]
    fn honeycomb_defaults_warn_on_unknown_protocol() {
        // We can't capture `tracing::warn!` without a subscriber here;
        // assert the *behaviour* — endpoint stays unset for an unknown
        // protocol.
        let out = compute_honeycomb_defaults(HoneycombEnvInput {
            api_key: Some("KEY".into()),
            existing_protocol: Some("smoke-signals".into()),
            ..Default::default()
        });
        assert_eq!(out.protocol, None); // user-provided, not overwritten
        assert_eq!(out.endpoint, None); // unknown protocol, no guess
        assert_eq!(out.headers.as_deref(), Some("x-honeycomb-team=KEY"));
    }
}
