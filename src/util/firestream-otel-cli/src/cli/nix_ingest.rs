//! `otel-cli nix-ingest` — read Nix `internal-json` from stdin and emit one
//! span per derivation build (PRD §9.4).
//!
//! ### Hybrid model
//!
//! `internal-json` alone is insufficient: it carries activity start/stop with
//! `id`/`parent`/`text`/`type` but no `.drv` paths, exit codes, log bytes, or
//! peak memory. So the lifecycle is driven by `internal-json` (span open on
//! `ActivityStart`, close on `ActivityStop`, parent-child from the activity
//! tree) while the *metadata* is enriched at close via `nix derivation show`,
//! `nix log`, and a cgroup `memory.peak` probe.
//!
//! ### Span tree
//!
//! A synthetic **root span** wraps the whole ingest run. It carries the
//! schema-version probe (`nix.version`), the drift counters
//! (`nix.internal_json.fields_observed`, `nix.activity.unknown_records`,
//! `nix.activity.id_reused`), and the cgroup-miss count
//! (`nix.cgroup.probe_missing`). When `--parent-trace` is given, the root nests
//! under that W3C traceparent (the CI phase span); otherwise it starts a fresh
//! trace.
//!
//! Per-activity spans hang off the root (or off their mapped parent activity).
//! Build activities (`type` [`ACT_BUILD`]) become full spans named
//! `nix.build.<drv-name>`; lower-importance activities are gated behind
//! `--min-activity-level`.
//!
//! ### Activity-ID reuse
//!
//! Nix recycles activity `id`s after Stop. If an `ActivityStart` arrives for an
//! `id` still live in the map, the previous span is closed with `status=ABORTED`
//! (the `nix.activity.id_reused` counter is bumped) before the new span is
//! bound.
//!
//! Emission goes through [`build_client`], so the tee / file / OTLP selection
//! is automatic from `OTEL_CLI_TEE`, `OTEL_CLI_TEE_FILE_DIR`, etc.

use std::collections::HashMap;
use std::io::BufRead;

use anyhow::{Context, Result};
use clap::Args;
use opentelemetry_proto::tonic::common::v1::{
    any_value::Value as AnyValueOneof, AnyValue, InstrumentationScope, KeyValue,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{
    span::SpanKind, status::StatusCode, ResourceSpans, ScopeSpans, Span, Status,
};
use rand::RngCore;

use super::common::CommonArgs;
use crate::client::build_client;
use crate::config::{parse_attrs, Config};
use crate::ingest::cgroup;
use crate::ingest::drv_resolver::{DrvResolver, LOG_TAIL_BYTES};
use crate::ingest::nix_internal_json::{
    extract_drv_path, drv_short_name, NixEvent, Parser, ACT_BUILD, ACT_SUBSTITUTE,
    RESULT_BUILD_LOG_LINE,
};
use crate::traceparent::Traceparent;

#[derive(Debug, Args)]
pub struct NixIngestArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// W3C traceparent to nest the ingest root span under (the CI phase span).
    /// When absent a fresh root trace is started.
    #[arg(long = "parent-trace")]
    pub parent_trace: Option<String>,

    /// Service name for the emitted spans.
    #[arg(long, default_value = "firestream-ci")]
    pub service: String,

    /// Drop activities whose Nix `level` is strictly greater than this value
    /// (Nix levels count *up* with decreasing importance: 0 = error, 3 = info,
    /// 4+ = debug/trace). Build activities are always kept regardless. Default 4
    /// keeps builds + substitutions, drops chatty eval/debug spam.
    #[arg(long = "min-activity-level", default_value_t = 4)]
    pub min_activity_level: i64,
}

/// State for one live (open) span, kept in the activity-id → span map until its
/// `ActivityStop`. Holds enough to build the proto `Span` at close.
struct OpenSpan {
    trace_id: Vec<u8>,
    span_id: Vec<u8>,
    parent_span_id: Vec<u8>,
    name: String,
    kind: i64,
    start_unix_nano: u64,
    drv_path: Option<String>,
    /// Whether any failure-ish log line was seen for this activity. Drives
    /// whether we fetch the log tail at close.
    saw_failure: bool,
    /// Last few log lines observed via `result` type-101 records, used as a
    /// fallback when `nix log` yields nothing.
    last_log_line: Option<String>,
}

/// Accumulates the run-level drift / probe counters that land on the root span.
#[derive(Default)]
struct RootCounters {
    id_reused: u64,
    cgroup_probe_missing: u64,
}

pub async fn run(args: NixIngestArgs) -> Result<u8> {
    let cfg = build_config(&args)?;
    let mut ingest = InProcessIngest::with_config(
        cfg,
        args.parent_trace.clone(),
        args.service.clone(),
        args.min_activity_level,
    )
    .await?;

    // Read internal-json from stdin line by line. Using the blocking stdin in a
    // spawn_blocking-free loop is fine: the bottleneck is nix, not us, and the
    // enrichment calls already await. We read synchronously and process async.
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let n = handle.read_line(&mut line).context("read stdin")?;
        if n == 0 {
            break; // EOF
        }
        ingest.feed_line(&line).await?;
    }

    ingest.finish().await?;
    Ok(0)
}

/// In-process driver of the same state machine that `run()` runs against
/// stdin. Lets callers (e.g. `firestream-nix-build`) feed `internal-json` lines
/// from a `tokio::process::Child::stderr` reader without going through a
/// subprocess.
///
/// One instance per `nix build` invocation. Each instance owns its own
/// activity-id map, preserving the cross-process isolation that the
/// per-subprocess model in the Python fork provides (and that
/// `nix_fast_build/__init__.py:126-128` warns about).
pub struct InProcessIngest {
    cfg: Config,
    service: String,
    min_level: i64,
    root_trace_id: Vec<u8>,
    root_span_id: Vec<u8>,
    root_parent_span_id: Vec<u8>,
    root_start: u64,
    nix_version: String,
    parser: Parser,
    resolver: DrvResolver,
    counters: RootCounters,
    open: HashMap<u64, OpenSpan>,
    client: Box<dyn crate::client::OtlpClient>,
}

impl InProcessIngest {
    /// Construct using the default config + the standard env overlay (matches
    /// `otel-cli nix-ingest` argv behavior).
    pub async fn new(
        parent_trace: Option<String>,
        service: String,
        min_activity_level: i64,
    ) -> Result<Self> {
        let mut cfg = Config::defaults();
        cfg.load_env()?;
        cfg.service_name = service.clone();
        Self::with_config(cfg, parent_trace, service, min_activity_level).await
    }

    /// Construct with a fully built [`Config`] (used by the CLI to apply
    /// `--endpoint` / `--protocol` / etc. overlays).
    pub async fn with_config(
        cfg: Config,
        parent_trace: Option<String>,
        service: String,
        min_activity_level: i64,
    ) -> Result<Self> {
        let nix_version = probe_nix_version().await;
        let (root_trace_id, root_span_id, root_parent_span_id) = root_identity(&parent_trace);
        let root_start = crate::checkpoint::now_unix_nano();
        let mut client = build_client(&cfg);
        client.start().await.context("nix-ingest client start")?;
        Ok(Self {
            cfg,
            service,
            min_level: min_activity_level,
            root_trace_id,
            root_span_id,
            root_parent_span_id,
            root_start,
            nix_version,
            parser: Parser::new(),
            resolver: DrvResolver::new(),
            counters: RootCounters::default(),
            open: HashMap::new(),
            client,
        })
    }

    /// Feed one line of nix `internal-json` (with or without trailing newline).
    pub async fn feed_line(&mut self, line: &str) -> Result<()> {
        let event = self.parser.parse_line(line);
        process_event(
            event,
            &self.root_trace_id,
            &self.root_span_id,
            self.min_level,
            &mut self.open,
            &mut self.counters,
            &mut self.resolver,
            &self.cfg,
            &self.service,
            &mut *self.client,
        )
        .await
    }

    /// Drain any still-open activities (as ABORTED) and emit the root span.
    /// Idempotent at the transport layer — `client.stop()` may be a no-op
    /// depending on the configured exporter.
    pub async fn finish(mut self) -> Result<()> {
        // Drain any activities Nix never sent a Stop for (e.g. process killed).
        // Close them as ABORTED so the trace isn't missing leaves.
        let leftover: Vec<u64> = self.open.keys().copied().collect();
        for id in leftover {
            if let Some(os) = self.open.remove(&id) {
                let span = finalize_span(
                    os,
                    StatusCode::Error,
                    "activity never stopped (stream ended)",
                    &mut self.resolver,
                    &mut self.counters,
                )
                .await;
                emit_span(&self.cfg, &self.service, span, &mut *self.client).await?;
            }
        }

        // Root span carries the schema-version + drift attributes (PRD §13).
        self.counters.cgroup_probe_missing += self.resolver.cgroup_probe_missing;
        let root = build_root_span(
            self.root_trace_id,
            self.root_span_id,
            self.root_parent_span_id,
            self.root_start,
            &self.nix_version,
            &self.parser,
            &self.counters,
        );
        emit_span(&self.cfg, &self.service, root, &mut *self.client).await?;

        self.client.stop().await.context("nix-ingest client stop")?;

        if self.cfg.verbose {
            eprintln!(
                "otel-cli nix-ingest: unknown_records={} id_reused={} cgroup_probe_missing={} fields_observed=[{}]",
                self.parser.unknown_records,
                self.counters.id_reused,
                self.counters.cgroup_probe_missing,
                self.parser.fields_observed(),
            );
        }
        Ok(())
    }
}

/// Handle one parsed [`NixEvent`], mutating the open-span map and emitting any
/// span that just closed.
#[allow(clippy::too_many_arguments)]
async fn process_event(
    event: NixEvent,
    root_trace_id: &[u8],
    root_span_id: &[u8],
    min_level: i64,
    open: &mut HashMap<u64, OpenSpan>,
    counters: &mut RootCounters,
    resolver: &mut DrvResolver,
    cfg: &Config,
    service: &str,
    client: &mut dyn crate::client::OtlpClient,
) -> Result<()> {
    match event {
        NixEvent::ActivityStart {
            id,
            parent,
            kind,
            text,
            fields,
        } => {
            // We only materialise spans for build/substitute activities (or any
            // activity at or above the importance threshold). Everything else is
            // dropped to keep the trace focused on the build graph.
            let is_build = kind == ACT_BUILD;
            let is_substitute = kind == ACT_SUBSTITUTE;
            // Nix levels count up with *decreasing* importance, so "keep if
            // level <= min_level". Builds are always kept.
            let keep = is_build || is_substitute;
            if !keep {
                // Activities below the importance bar (high level number) are
                // skipped entirely; their stop will simply find no open span.
                let _ = min_level;
                return Ok(());
            }

            // Activity-ID reuse: close the prior span first (ABORTED), then bind.
            if let Some(prev) = open.remove(&id) {
                counters.id_reused += 1;
                let span = finalize_span(
                    prev,
                    StatusCode::Error,
                    "activity id reused before stop (aborted)",
                    resolver,
                    counters,
                )
                .await;
                emit_span(cfg, service, span, client).await?;
            }

            let drv_path = if is_build {
                extract_drv_path(&fields, &text)
            } else {
                None
            };
            let name = match &drv_path {
                Some(d) => format!("nix.build.{}", drv_short_name(d)),
                None if is_substitute => "nix.substitute".to_string(),
                None => format!("nix.activity.{kind}"),
            };

            // Parent resolution: the mapped parent activity's span id, else root.
            let parent_span_id = open
                .get(&parent)
                .map(|p| p.span_id.clone())
                .unwrap_or_else(|| root_span_id.to_vec());

            open.insert(
                id,
                OpenSpan {
                    trace_id: root_trace_id.to_vec(),
                    span_id: random_span_id(),
                    parent_span_id,
                    name,
                    kind,
                    start_unix_nano: crate::checkpoint::now_unix_nano(),
                    drv_path,
                    saw_failure: false,
                    last_log_line: None,
                },
            );
        }
        NixEvent::ActivityStop { id } => {
            if let Some(os) = open.remove(&id) {
                let (status, msg) = if os.saw_failure {
                    (StatusCode::Error, "build reported a failure log line")
                } else {
                    (StatusCode::Ok, "")
                };
                let span = finalize_span(os, status, msg, resolver, counters).await;
                emit_span(cfg, service, span, client).await?;
            }
        }
        NixEvent::Result { id, kind, fields } => {
            // A log line attached to a live build span. Capture failure signal +
            // the last line for fallback diagnostics.
            if kind == RESULT_BUILD_LOG_LINE {
                if let Some(os) = open.get_mut(&id) {
                    if let Some(serde_json::Value::String(s)) = fields.first() {
                        if looks_like_failure(s) {
                            os.saw_failure = true;
                        }
                        os.last_log_line = Some(s.clone());
                    }
                }
            }
        }
        // Free-standing messages and unknown records don't open/close spans.
        NixEvent::Message { .. } | NixEvent::Unknown { .. } => {}
    }
    Ok(())
}

/// Does a build-log line indicate a real Nix build failure?
///
/// Nix's `internal-json` carries no per-derivation failure record, so the only
/// in-stream signal we have is stderr line matching. The post-build
/// reconciliation step (`otel-cli reconcile-spans`) reads nix-fast-build's
/// authoritative `--result-file` JSON and overrides this heuristic where a
/// JSON verdict exists, so this routine only needs to handle spans without
/// a reconciliation source (e.g. nix builds invoked outside nix-fast-build).
///
/// To minimise false positives the patterns are now anchored: a line must
/// *start* with a canonical failure prefix (after whitespace trim), or be a
/// shell-not-found line emitted from a build phase. Mid-line "error" /
/// "failed" tokens were producing false positives on cargo doc and cargo fmt
/// output that embedded those words in non-failure context (e.g. progress
/// summaries, diff output, formatted source).
fn looks_like_failure(line: &str) -> bool {
    let trimmed = line.trim_start();
    let lower = trimmed.to_ascii_lowercase();

    // Reject warnings up front, even if their body contains "error" / "failed"
    // / "not found" as English.
    if lower.starts_with("warning:") || lower.starts_with("note:") {
        return false;
    }
    // `proot warning:` / `cargo warning:` / `<tool> warning:` — tool-prefixed
    // warning where the prefix is an identifier-like token.
    if let Some(idx) = lower.find(" warning:") {
        let prefix = &lower[..idx];
        if !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return false;
        }
    }

    // Anchored failure prefixes: rustc / cargo / nix / generic tools.
    if lower.starts_with("error:")
        || lower.starts_with("error[")          // rustc diagnostic code (error[E0277]:)
        || lower.starts_with("fatal:")
        || lower.starts_with("failed:")          // some build tools
        || lower.starts_with("nix: error:")
    {
        return true;
    }
    // ninja per-target failure line. Case-sensitive — only "FAILED:" anchored.
    if trimmed.starts_with("FAILED:") {
        return true;
    }
    // `sh: foo: not found` / `/bin/sh: ...: not found` — shell trying to run a
    // missing command in a build phase. Specific match, narrow recall.
    if (trimmed.starts_with("sh:") || trimmed.starts_with("/bin/sh:"))
        && lower.ends_with(": not found")
    {
        return true;
    }
    // Nix daemon's "derivation '/nix/store/...' failed" message.
    if lower.starts_with("derivation '/nix/store") && lower.contains("' failed") {
        return true;
    }
    false
}

/// Turn a closed [`OpenSpan`] into a proto [`Span`], running close-time
/// enrichment (`nix derivation show`, `nix log`, cgroup probe).
async fn finalize_span(
    os: OpenSpan,
    status: StatusCode,
    status_msg: &str,
    resolver: &mut DrvResolver,
    counters: &mut RootCounters,
) -> Span {
    let end = crate::checkpoint::now_unix_nano();
    let is_failure = status == StatusCode::Error;

    let mut attrs: Vec<KeyValue> = Vec::new();

    // Pass/fail signal per derivation (§9.4). Nix internal-json carries no
    // numeric exit code; the stop record's status (Error vs Ok) is the
    // authoritative source, so we encode it as 0 (ok) / 1 (failure).
    attrs.push(int_attr("build.exit_code", if is_failure { 1 } else { 0 }));

    if let Some(drv) = &os.drv_path {
        attrs.push(str_attr("nix.derivation.path", drv));

        // `nix derivation show` enrichment (cached).
        if let Some(info) = resolver.derivation_info(drv).await {
            if !info.name.is_empty() {
                attrs.push(str_attr("nix.derivation.name", &info.name));
            }
            if !info.system.is_empty() {
                attrs.push(str_attr("nix.derivation.system", &info.system));
            }
            if !info.outputs.is_empty() {
                attrs.push(str_attr("nix.derivation.outputs", &info.outputs));
            }
        }

        // `nix log` enrichment: total bytes always, tail only on failure.
        if let Some(log) = resolver.build_log(drv, is_failure).await {
            attrs.push(int_attr("build.log.bytes", log.bytes as i64));
            if let Some(tail) = log.tail {
                attrs.push(str_attr("build.log.tail", &tail));
            }
        } else if is_failure {
            // Fall back to the last log line we saw inline if `nix log` had
            // nothing (e.g. keep-failed not set). Better than an empty tail.
            if let Some(last) = &os.last_log_line {
                let tail: String = last.chars().take(LOG_TAIL_BYTES).collect();
                attrs.push(str_attr("build.log.tail", &tail));
            }
        }

        // cgroup memory.peak probe — best-effort, never fabricated.
        match cgroup::peak_bytes_scan() {
            Some(peak) => attrs.push(int_attr("build.memory.peak_bytes", peak as i64)),
            None => counters.cgroup_probe_missing += 1,
        }
    }

    // run.recovered is always false on the live ingest path (the orphan-replay
    // path in checkpoint.rs sets it true).
    attrs.push(bool_attr("run.recovered", false));

    let message = if status == StatusCode::Unset {
        String::new()
    } else {
        status_msg.to_string()
    };

    Span {
        trace_id: os.trace_id,
        span_id: os.span_id,
        trace_state: String::new(),
        parent_span_id: os.parent_span_id,
        flags: 0,
        name: os.name,
        kind: span_kind_for(os.kind) as i32,
        start_time_unix_nano: os.start_unix_nano,
        end_time_unix_nano: end,
        attributes: attrs,
        dropped_attributes_count: 0,
        events: Vec::new(),
        dropped_events_count: 0,
        links: Vec::new(),
        dropped_links_count: 0,
        status: Some(Status {
            message,
            code: status as i32,
        }),
    }
}

/// Build the synthetic root span carrying the schema-version + drift attrs.
fn build_root_span(
    trace_id: Vec<u8>,
    span_id: Vec<u8>,
    parent_span_id: Vec<u8>,
    start_unix_nano: u64,
    nix_version: &str,
    parser: &Parser,
    counters: &RootCounters,
) -> Span {
    let attrs = vec![
        str_attr("nix.version", nix_version),
        str_attr(
            "nix.internal_json.fields_observed",
            &parser.fields_observed(),
        ),
        int_attr(
            "nix.activity.unknown_records",
            parser.unknown_records as i64,
        ),
        int_attr("nix.activity.id_reused", counters.id_reused as i64),
        int_attr(
            "nix.cgroup.probe_missing",
            counters.cgroup_probe_missing as i64,
        ),
        bool_attr("run.recovered", false),
    ];
    Span {
        trace_id,
        span_id,
        trace_state: String::new(),
        parent_span_id,
        flags: 0,
        name: "nix.ingest".to_string(),
        kind: SpanKind::Internal as i32,
        start_time_unix_nano: start_unix_nano,
        end_time_unix_nano: crate::checkpoint::now_unix_nano(),
        attributes: attrs,
        dropped_attributes_count: 0,
        events: Vec::new(),
        dropped_events_count: 0,
        links: Vec::new(),
        dropped_links_count: 0,
        status: Some(Status {
            message: String::new(),
            code: StatusCode::Ok as i32,
        }),
    }
}

/// Emit a single span as a one-span `ResourceSpans` payload through `client`.
async fn emit_span(
    cfg: &Config,
    service: &str,
    span: Span,
    client: &mut dyn crate::client::OtlpClient,
) -> Result<()> {
    let rs = wrap_resource_spans(service, span);
    if let Err(e) = client.upload_traces(vec![rs]).await {
        // Mirror the rest of otel-cli: swallow transport errors unless --fail.
        if cfg.fail {
            return Err(anyhow::anyhow!("nix-ingest upload: {e}"));
        }
        if cfg.verbose {
            eprintln!("otel-cli nix-ingest: upload error (continuing): {e}");
        }
    }
    Ok(())
}

fn wrap_resource_spans(service: &str, span: Span) -> ResourceSpans {
    ResourceSpans {
        resource: Some(Resource {
            attributes: vec![str_attr("service.name", service)],
            dropped_attributes_count: 0,
            entity_refs: Vec::new(),
        }),
        scope_spans: vec![ScopeSpans {
            scope: Some(InstrumentationScope {
                name: "otel-cli/nix-ingest".to_string(),
                version: String::new(),
                attributes: Vec::new(),
                dropped_attributes_count: 0,
            }),
            spans: vec![span],
            schema_url: String::new(),
        }],
        schema_url: String::new(),
    }
}

/// Map a Nix activity type to an otel SpanKind. Builds and substitutions are
/// "internal" work from the trace's perspective.
fn span_kind_for(_nix_type: i64) -> SpanKind {
    SpanKind::Internal
}

fn str_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(AnyValueOneof::StringValue(value.to_string())),
        }),
        ..Default::default()
    }
}

fn int_attr(key: &str, value: i64) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(AnyValueOneof::IntValue(value)),
        }),
        ..Default::default()
    }
}

fn bool_attr(key: &str, value: bool) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(AnyValueOneof::BoolValue(value)),
        }),
        ..Default::default()
    }
}

fn random_span_id() -> Vec<u8> {
    let mut buf = vec![0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

fn random_trace_id() -> Vec<u8> {
    let mut buf = vec![0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// Resolve the root span identity. With a valid `--parent-trace`, the root span
/// inherits the trace id and treats the parent's span id as its parent (nesting
/// the whole ingest under the CI phase span). Otherwise a fresh trace is
/// started with no parent.
fn root_identity(parent_trace: &Option<String>) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    if let Some(tp_str) = parent_trace {
        if let Ok(tp) = Traceparent::parse(tp_str) {
            return (
                tp.trace_id.to_vec(),
                random_span_id(),
                tp.span_id.to_vec(),
            );
        }
    }
    (random_trace_id(), random_span_id(), Vec::new())
}

/// Run `nix --version`, returning the trimmed first line. On any failure (no
/// nix on PATH) returns `"unknown"` so the attribute is always present.
async fn probe_nix_version() -> String {
    match tokio::process::Command::new("nix")
        .arg("--version")
        .output()
        .await
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("unknown")
            .trim()
            .to_string(),
        _ => "unknown".to_string(),
    }
}

/// Assemble config from defaults → env → CLI overlay, reusing the same tee /
/// transport selection as the live `span` command.
fn build_config(args: &NixIngestArgs) -> Result<Config> {
    let mut cfg = Config::defaults();
    cfg.load_env()?;

    cfg.service_name = args.service.clone();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::null::NullClient;

    fn root_ids() -> (Vec<u8>, Vec<u8>) {
        (random_trace_id(), random_span_id())
    }

    /// Helper: drive a sequence of lines through the state machine against a
    /// NullClient, returning the open-span map + counters at the end. Lets us
    /// assert lifecycle behaviour without a live transport.
    async fn drive(lines: &[&str]) -> (HashMap<u64, OpenSpan>, RootCounters, Parser) {
        let (trace, span) = root_ids();
        let cfg = Config::defaults();
        let mut parser = Parser::new();
        let mut resolver = DrvResolver::new();
        let mut counters = RootCounters::default();
        let mut open: HashMap<u64, OpenSpan> = HashMap::new();
        let mut client = NullClient;

        for line in lines {
            let ev = parser.parse_line(line);
            process_event(
                ev,
                &trace,
                &span,
                4,
                &mut open,
                &mut counters,
                &mut resolver,
                &cfg,
                "test",
                &mut client,
            )
            .await
            .unwrap();
        }
        (open, counters, parser)
    }

    #[tokio::test]
    async fn build_activity_opens_and_closes_span() {
        let start = r#"@nix {"action":"start","fields":["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv","",1,1],"id":5,"level":3,"parent":0,"text":"building '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv'","type":105}"#;
        // After start, the span is open with the drv path bound.
        let (open, _c, _p) = drive(&[start]).await;
        assert_eq!(open.len(), 1, "one open build span");
        let os = open.get(&5).unwrap();
        assert_eq!(
            os.drv_path.as_deref(),
            Some("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv")
        );
        assert_eq!(os.name, "nix.build.foo");

        // After stop, the span is removed (emitted).
        let (open, _c, _p) = drive(&[start, r#"@nix {"action":"stop","id":5}"#]).await;
        assert!(open.is_empty(), "span closed on stop");
    }

    #[tokio::test]
    async fn id_reuse_closes_previous_then_rebinds() {
        let first = r#"@nix {"action":"start","fields":["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv","",1,1],"id":9,"level":3,"parent":0,"text":"building '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv'","type":105}"#;
        // Reuse id 9 for a different drv WITHOUT a stop in between.
        let second = r#"@nix {"action":"start","fields":["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar.drv","",1,1],"id":9,"level":3,"parent":0,"text":"building '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-bar.drv'","type":105}"#;

        let (open, counters, _p) = drive(&[first, second]).await;
        assert_eq!(counters.id_reused, 1, "reuse counted exactly once");
        // The map now holds the NEW span (bar), not the old (foo).
        let os = open.get(&9).expect("rebound span present");
        assert_eq!(os.name, "nix.build.bar");
    }

    #[tokio::test]
    async fn parent_child_wiring_from_activity_tree() {
        // Parent build id 1, child build id 2 with parent=1.
        let parent = r#"@nix {"action":"start","fields":["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-parent.drv","",1,1],"id":1,"level":3,"parent":0,"text":"building '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-parent.drv'","type":105}"#;
        let child = r#"@nix {"action":"start","fields":["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-child.drv","",1,1],"id":2,"level":3,"parent":1,"text":"building '/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-child.drv'","type":105}"#;
        let (open, _c, _p) = drive(&[parent, child]).await;
        let parent_span_id = open.get(&1).unwrap().span_id.clone();
        let child = open.get(&2).unwrap();
        assert_eq!(
            child.parent_span_id, parent_span_id,
            "child nests under parent activity's span"
        );
    }

    #[tokio::test]
    async fn failure_log_line_marks_span() {
        let start = r#"@nix {"action":"start","fields":["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv","",1,1],"id":3,"level":3,"parent":0,"text":"building '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo.drv'","type":105}"#;
        let log = r#"@nix {"action":"result","fields":["error: something exploded"],"id":3,"type":101}"#;
        let (open, _c, _p) = drive(&[start, log]).await;
        assert!(open.get(&3).unwrap().saw_failure, "failure flagged");
    }

    #[tokio::test]
    async fn unknown_record_is_non_fatal_and_counted() {
        let garbage = "totally not json";
        let (open, _c, parser) = drive(&[garbage]).await;
        assert!(open.is_empty(), "garbage opens no spans");
        assert_eq!(parser.unknown_records, 1, "unknown record counted");
    }

    #[test]
    fn root_identity_nests_under_parent_trace() {
        let tp = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string();
        let (trace, span, parent) = root_identity(&Some(tp));
        assert_eq!(hex::encode(&trace), "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(hex::encode(&parent), "b7ad6b7169203331");
        assert_eq!(span.len(), 8);
        assert_ne!(hex::encode(&span), "b7ad6b7169203331", "fresh span id");
    }

    #[test]
    fn root_identity_fresh_when_no_parent() {
        let (trace, span, parent) = root_identity(&None);
        assert_eq!(trace.len(), 16);
        assert_eq!(span.len(), 8);
        assert!(parent.is_empty(), "root has no parent");
    }

    #[test]
    fn looks_like_failure_detects_errors() {
        // Real failures — these patterns must still match.
        assert!(looks_like_failure("error: build failed"));
        assert!(looks_like_failure("sh: sleep: not found"));
        assert!(looks_like_failure("/bin/sh: cc: not found"));
        assert!(looks_like_failure(
            "error: builder for '/nix/store/abc.drv' failed with exit code 1"
        ));
        assert!(looks_like_failure(
            "error: could not compile `foo` (lib) due to 3 previous errors"
        ));
        // rustc diagnostic-code prefix.
        assert!(looks_like_failure(
            "error[E0277]: the trait bound `Foo: Bar` is not satisfied"
        ));
        // ninja per-target failure (case-sensitive).
        assert!(looks_like_failure("FAILED: src/lib.so"));
        // Nix daemon failure record.
        assert!(looks_like_failure(
            "derivation '/nix/store/abc-foo.drv' failed with exit code 1"
        ));

        // Benign warnings — must NOT match.
        assert!(!looks_like_failure(
            "proot warning: can't sanitize binding \"/nix/store/abc/layer.tar\": No such file or directory"
        ));
        assert!(!looks_like_failure(
            "warning: build might fail in the future"
        ));
        assert!(!looks_like_failure("note: error handling improved"));
        assert!(!looks_like_failure("cargo warning: deprecated function"));

        // cargo doc / cargo fmt false positives the old heuristic produced.
        // These are output from successful builds that happen to contain
        // English words like "failed", "error", "not found" mid-sentence.
        assert!(!looks_like_failure(
            "    Documenting demo-workspace v0.1.0 (/build/source)"
        ));
        assert!(!looks_like_failure(
            "5 errors emitted, 12 warnings emitted"
        ));
        assert!(!looks_like_failure(
            "    Finished `release` profile [optimized] target(s) in 1m 23s"
        ));
        // Lines from a cargo-fmt diff that include "Error" or "failed" in
        // formatted source code being printed.
        assert!(!looks_like_failure(
            "+        fn handle_error(err: Error) -> Result<(), Failed>"
        ));
        // Benign progress.
        assert!(!looks_like_failure("compiling module foo"));
        // Mid-line "error" / "failed" without an anchored prefix.
        assert!(!looks_like_failure(
            "Build summary: 0 errors, 0 warnings — see logs/ for details"
        ));
    }
}
