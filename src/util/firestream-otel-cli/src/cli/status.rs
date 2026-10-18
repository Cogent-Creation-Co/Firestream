//! `otel-cli status` — send canary spans and emit a diagnostic report.
//!
//! Go reference: `otelcli/status.go`. Unlike Go, we don't emit the legacy
//! `spans` array or per-canary `errors` list — instead the global
//! [`Diagnostics`] singleton is the source of truth, and we report only the
//! last canary's identity under `span_data`. This is the diagnostic surface
//! users need to debug why their spans aren't being exported.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;

use super::common::CommonArgs;
use crate::client::build_client;
use crate::config::{parse_attrs, parse_duration, Config};
use crate::diagnostics::{self, Diagnostics};
use crate::span as span_builder;
use crate::traceparent::Traceparent;

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Service name attribute (also used as the canary span service).
    #[arg(long, short = 's', env = "OTEL_CLI_SERVICE_NAME", default_value = "otel-cli")]
    pub service: String,

    /// Number of canary spans to send.
    #[arg(long, default_value_t = 1)]
    pub canary_count: u32,

    /// Sleep interval between canary spans (e.g. "1s"). Empty = no sleep.
    #[arg(long, default_value = "")]
    pub canary_interval: String,

    /// Ignore the TRACEPARENT env var when building canaries.
    #[arg(long = "tp-ignore-env")]
    pub tp_ignore_env: bool,

    /// Path where the JSON-file client writes spans (when --protocol json+file).
    #[arg(long)]
    pub json_dir: Option<String>,

    /// Skip TLS verification (also sets the `insecure_skip_verify` diag flag).
    #[arg(long = "tls-no-verify")]
    pub tls_no_verify: bool,
}

/// Top-level JSON shape printed to stdout. Field names match Go's
/// `StatusOutput` so external scripts can keep parsing the same keys.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub config: Config,
    pub diagnostics: Diagnostics,
    pub env: BTreeMap<String, String>,
    pub cli_args: Vec<String>,
    pub span_data: SpanData,
}

/// Identity of the last canary span, for downstream traceparent propagation.
#[derive(Debug, Default, Serialize)]
pub struct SpanData {
    pub trace_id: String,
    pub span_id: String,
    pub traceparent: String,
    pub is_sampled: bool,
}

pub async fn run(args: StatusArgs) -> Result<u8> {
    // 1. Assemble config (same layered pattern as span/exec).
    let cfg = build_config(&args).context("building status config")?;

    // 2. Update the global diagnostics handle so the JSON output reflects this
    //    run's view of the world. Done before sending canaries so even a hard
    //    transport failure still produces a useful report.
    let cli_args: Vec<String> = std::env::args().collect();
    let timeout_ms = cfg
        .parse_timeout()
        .unwrap_or(Duration::ZERO)
        .as_millis() as i64;
    let endpoint = effective_endpoint(&cfg);
    let endpoint_source = endpoint_source(&cfg);

    if let Ok(mut diag) = diagnostics::global().lock() {
        diag.is_recording = cfg.is_recording();
        diag.endpoint = endpoint.clone();
        diag.endpoint_source = endpoint_source;
        diag.insecure_skip_verify = cfg.tls_no_verify;
        diag.parsed_timeout_ms = timeout_ms;
        diag.config_file_loaded = !cfg.cfg_file.is_empty();
        diag.number_of_args = cli_args.len() as i32;
        diag.cli_args = cli_args.clone();
        diag.detected_localhost = detect_localhost(&endpoint);
    }

    // 3. Fire canaries. Errors are captured via Diagnostics rather than aborting
    //    — `status` is diagnostic, so we always print a report.
    let interval = parse_duration(&args.canary_interval).unwrap_or(Duration::ZERO);
    let mut last_tp: Option<Traceparent> = None;

    for n in 0..args.canary_count {
        // Each canary is its own resource span; we customize the name per index
        // so a user can correlate them in the receiver.
        let mut canary_cfg = cfg.clone();
        canary_cfg.span_name = if n == 0 {
            "otel-cli status".to_string()
        } else {
            format!("otel-cli status canary {n}")
        };
        canary_cfg.kind = "internal".to_string();

        // Chain each canary to the previous one: same trace, parent = previous
        // span id. This produces a small tree the user can verify on the wire.
        if let Some(prev) = &last_tp {
            canary_cfg.force_trace_id = hex::encode(prev.trace_id);
            canary_cfg.force_parent_span_id = hex::encode(prev.span_id);
        }

        let resource_spans = span_builder::build_resource_spans(&canary_cfg);

        let mut client = build_client(&canary_cfg);
        let send_result: Result<()> = async {
            client.start().await.context("client start")?;
            client
                .upload_traces(vec![resource_spans.clone()])
                .await
                .context("upload traces")?;
            client.stop().await.context("client stop")?;
            Ok(())
        }
        .await;

        if let Err(ref err) = send_result {
            // capture error in diagnostics, then keep going
            Diagnostics::set_error(&format!("{err:#}"));
        }

        // Record this canary as "last" for the report — even if the send
        // failed, the span id is still useful for chaining diagnostics.
        let span = &resource_spans.scope_spans[0].spans[0];
        last_tp = Some(span_to_traceparent(
            &span.trace_id,
            &span.span_id,
            cfg.is_recording(),
        ));

        // sleep between canaries, but never after the last one
        if !args.canary_interval.is_empty() && n + 1 < args.canary_count {
            tokio::time::sleep(interval).await;
        }
    }

    // 4. Build + emit the JSON report.
    let report = build_status_report(
        &cfg,
        diagnostics::snapshot(),
        collect_env(|k| std::env::var(k)),
        cli_args,
        last_tp,
    );

    let json = serde_json::to_string_pretty(&report)
        .context("serializing status report to JSON")?;
    println!("{json}");

    // 5. status is diagnostic — always exit 0 so users can pipe the JSON
    //    without worrying about exit codes.
    Ok(0)
}

/// Walk the precedence chain: defaults < file < env < CLI flags.
fn build_config(args: &StatusArgs) -> Result<Config> {
    let mut cfg = Config::defaults();

    if let Some(path) = args.common.config_file.as_deref() {
        if !path.is_empty() {
            cfg.load_file(path)
                .with_context(|| format!("loading config file {path:?}"))?;
        }
    }

    cfg.load_env().context("loading env vars")?;

    cfg.service_name = args.service.clone();
    cfg.status_canary_count = args.canary_count;
    cfg.status_canary_interval = args.canary_interval.clone();
    if args.tp_ignore_env {
        cfg.traceparent_ignore_env = true;
    }

    // Transport overlay from CommonArgs
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
    if args.tls_no_verify {
        cfg.tls_no_verify = true;
    }

    if let Some(v) = &args.json_dir {
        cfg.json_dir = v.clone();
    }

    Ok(cfg)
}

/// True when the endpoint string points at the local loopback. Match what
/// the OTel spec considers "localhost" — both DNS name and v4 literal.
pub fn detect_localhost(endpoint: &str) -> bool {
    let lower = endpoint.to_ascii_lowercase();
    lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("[::1]")
}

/// Which endpoint will actually be used (signal-specific overrides general).
fn effective_endpoint(cfg: &Config) -> String {
    if !cfg.traces_endpoint.is_empty() {
        cfg.traces_endpoint.clone()
    } else {
        cfg.endpoint.clone()
    }
}

/// Best-effort label for *why* this endpoint was picked. Matches the
/// human-readable hints Go's `status` emits.
fn endpoint_source(cfg: &Config) -> String {
    if !cfg.traces_endpoint.is_empty() {
        "traces_endpoint".to_string()
    } else if !cfg.endpoint.is_empty() {
        "endpoint".to_string()
    } else if !cfg.json_dir.is_empty() || cfg.protocol == "json+file" {
        "json_dir".to_string()
    } else {
        "unset".to_string()
    }
}

/// Collect every `OTEL_*` env variable via a closure. Pulling the getter out
/// makes this trivially testable without mutating process env.
pub fn collect_env<F>(getter: F) -> BTreeMap<String, String>
where
    F: Fn(&str) -> Result<String, std::env::VarError>,
{
    // The set of env-var names otel-cli reads (mirrors load_env_with's table).
    const KNOWN: &[&str] = &[
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_PROTOCOL",
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
        "OTEL_EXPORTER_OTLP_TIMEOUT",
        "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT",
        "OTEL_EXPORTER_OTLP_HEADERS",
        "OTEL_EXPORTER_OTLP_INSECURE",
        "OTEL_EXPORTER_OTLP_BLOCKING",
        "OTEL_EXPORTER_OTLP_CERTIFICATE",
        "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
        "OTEL_EXPORTER_OTLP_CLIENT_KEY",
        "OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY",
        "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
        "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE",
        "OTEL_CLI_TLS_NO_VERIFY",
        "OTEL_CLI_NO_TLS_VERIFY",
        "OTEL_CLI_SERVICE_NAME",
        "OTEL_SERVICE_NAME",
        "OTEL_CLI_SPAN_NAME",
        "OTEL_CLI_TRACE_KIND",
        "OTEL_CLI_ATTRIBUTES",
        "OTEL_CLI_STATUS_CODE",
        "OTEL_CLI_STATUS_DESCRIPTION",
        "OTEL_CLI_FORCE_SPAN_ID",
        "OTEL_CLI_FORCE_PARENT_SPAN_ID",
        "OTEL_CLI_FORCE_TRACE_ID",
        "OTEL_CLI_CARRIER_FILE",
        "OTEL_CLI_IGNORE_ENV",
        "OTEL_CLI_PRINT_TRACEPARENT",
        "OTEL_CLI_EXPORT_TRACEPARENT",
        "OTEL_CLI_TRACEPARENT_REQUIRED",
        "OTEL_CLI_EXEC_CMD_TIMEOUT",
        "OTEL_CLI_EXEC_TP_DISABLE_INJECT",
        "OTEL_CLI_CONFIG_FILE",
        "OTEL_CLI_VERBOSE",
        "OTEL_CLI_FAIL",
        "TRACEPARENT",
    ];

    let mut out = BTreeMap::new();
    for name in KNOWN {
        match getter(name) {
            Ok(v) if !v.is_empty() => {
                // Redact anything header-shaped — headers commonly carry
                // bearer tokens.
                let value = if *name == "OTEL_EXPORTER_OTLP_HEADERS"
                    || name.to_ascii_lowercase().contains("token")
                {
                    "--- redacted ---".to_string()
                } else {
                    v
                };
                out.insert((*name).to_string(), value);
            }
            _ => {}
        }
    }
    out
}

/// Assemble the report from the four already-computed inputs. Pure function
/// so unit tests can exercise the serialization shape.
pub fn build_status_report(
    cfg: &Config,
    diag: Diagnostics,
    env: BTreeMap<String, String>,
    cli_args: Vec<String>,
    last_tp: Option<Traceparent>,
) -> StatusReport {
    let span_data = match last_tp {
        Some(tp) => SpanData {
            trace_id: tp.trace_id_string(),
            span_id: tp.span_id_string(),
            traceparent: tp.encode(),
            is_sampled: tp.is_sampled(),
        },
        None => SpanData::default(),
    };

    StatusReport {
        config: cfg.clone(),
        diagnostics: diag,
        env,
        cli_args,
        span_data,
    }
}

/// Wrap raw trace/span id bytes from a built `Span` into a `Traceparent`.
fn span_to_traceparent(trace_bytes: &[u8], span_bytes: &[u8], recording: bool) -> Traceparent {
    let mut trace_id = [0u8; 16];
    let mut span_id = [0u8; 8];
    let tlen = trace_id.len().min(trace_bytes.len());
    trace_id[..tlen].copy_from_slice(&trace_bytes[..tlen]);
    let slen = span_id.len().min(span_bytes.len());
    span_id[..slen].copy_from_slice(&span_bytes[..slen]);
    Traceparent {
        version: 0,
        trace_id,
        span_id,
        flags: if recording { 0x01 } else { 0x00 },
        initialized: recording,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::VarError;

    #[test]
    fn detect_localhost_from_endpoint() {
        assert!(detect_localhost("localhost:4317"));
        assert!(detect_localhost("http://localhost"));
        assert!(detect_localhost("https://LOCALHOST:4318"));
        assert!(detect_localhost("127.0.0.1:4317"));
        assert!(detect_localhost("http://127.0.0.1:4318/v1/traces"));
        assert!(detect_localhost("[::1]:4317"));
        // negatives
        assert!(!detect_localhost(""));
        assert!(!detect_localhost("otel-collector.svc.cluster.local:4317"));
        assert!(!detect_localhost("example.com:4317"));
    }

    #[test]
    fn collect_otel_env_vars_filters_correctly() {
        // Fake env: a few OTEL_* values, plus noise that should not appear.
        let env = |k: &str| -> Result<String, VarError> {
            match k {
                "OTEL_EXPORTER_OTLP_ENDPOINT" => Ok("http://localhost:4318".to_string()),
                "OTEL_CLI_SERVICE_NAME" => Ok("svc".to_string()),
                "OTEL_EXPORTER_OTLP_HEADERS" => Ok("Authorization=Bearer xyz".to_string()),
                "HOME" => Ok("/home/test".to_string()),
                _ => Err(VarError::NotPresent),
            }
        };
        let got = collect_env(env);
        assert_eq!(
            got.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
            Some(&"http://localhost:4318".to_string())
        );
        assert_eq!(got.get("OTEL_CLI_SERVICE_NAME"), Some(&"svc".to_string()));
        // headers are redacted
        assert_eq!(
            got.get("OTEL_EXPORTER_OTLP_HEADERS"),
            Some(&"--- redacted ---".to_string())
        );
        // non-OTEL vars excluded
        assert!(!got.contains_key("HOME"));
    }

    #[test]
    fn collect_env_skips_empty_values() {
        let env = |k: &str| -> Result<String, VarError> {
            if k == "OTEL_CLI_SERVICE_NAME" {
                Ok(String::new())
            } else {
                Err(VarError::NotPresent)
            }
        };
        assert!(collect_env(env).is_empty());
    }

    #[test]
    fn build_status_report_serializes_cleanly() {
        let cfg = Config::defaults();
        let diag = Diagnostics {
            is_recording: false,
            ..Default::default()
        };
        let mut env = BTreeMap::new();
        env.insert(
            "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
            "http://localhost:4318".to_string(),
        );
        let cli_args = vec!["otel-cli".to_string(), "status".to_string()];
        let tp =
            Traceparent::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01").unwrap();

        let report = build_status_report(&cfg, diag, env, cli_args, Some(tp));
        let json = serde_json::to_string(&report).expect("serialises");

        // All five top-level keys must appear.
        for key in [
            "\"config\"",
            "\"diagnostics\"",
            "\"env\"",
            "\"cli_args\"",
            "\"span_data\"",
        ] {
            assert!(json.contains(key), "missing key {key} in {json}");
        }
        // span_data round-trip
        assert!(json.contains("0af7651916cd43dd8448eb211c80319c"));
        assert!(json.contains("b7ad6b7169203331"));
        assert!(json.contains("\"is_sampled\":true"));
    }

    #[test]
    fn build_status_report_handles_missing_last_span() {
        let cfg = Config::defaults();
        let diag = Diagnostics::default();
        let env = BTreeMap::new();
        let cli_args = vec!["otel-cli".to_string()];
        let report = build_status_report(&cfg, diag, env, cli_args, None);
        assert_eq!(report.span_data.trace_id, "");
        assert_eq!(report.span_data.span_id, "");
        assert!(!report.span_data.is_sampled);
    }
}
