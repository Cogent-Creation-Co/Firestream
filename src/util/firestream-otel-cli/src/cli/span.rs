//! `otel-cli span` and subcommands.
//!
//! Go references: `otelcli/span.go`, `span_background.go`, `span_event.go`,
//! `span_end.go`.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use opentelemetry_proto::tonic::common::v1::KeyValue;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;

use super::common::CommonArgs;
use crate::client::build_client;
use crate::config::{parse_attrs, Config};
use crate::diagnostics;
use crate::memory;
use crate::span as span_builder;
use crate::traceparent::Traceparent;

#[derive(Debug, Args)]
pub struct SpanArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Service name attribute.
    #[arg(long, short = 's', env = "OTEL_CLI_SERVICE_NAME", default_value = "otel-cli")]
    pub service: String,

    /// Span name.
    #[arg(long, short = 'n', env = "OTEL_CLI_SPAN_NAME")]
    pub name: Option<String>,

    /// Span kind: internal | server | client | producer | consumer.
    #[arg(long, short = 'k', env = "OTEL_CLI_TRACE_KIND", default_value = "client")]
    pub kind: String,

    /// Span start time (RFC3339, RFC3339Nano, Unix epoch, or "now").
    #[arg(long, default_value = "now")]
    pub start: String,

    /// Span end time (RFC3339, RFC3339Nano, Unix epoch, or "now").
    #[arg(long, default_value = "now")]
    pub end: String,

    /// Attributes as "k1=v1,k2=v2". Repeatable: each `--attrs` flag is appended
    /// to the merged set, and the value of each flag may itself be a
    /// comma-separated list. This matches OTEL_RESOURCE_ATTRIBUTES semantics.
    #[arg(long, short = 'a', action = clap::ArgAction::Append)]
    pub attrs: Vec<String>,

    /// Span status code: unset | ok | error.
    #[arg(long, default_value = "unset")]
    pub status_code: String,

    /// Span status description.
    #[arg(long, default_value = "")]
    pub status_description: String,

    /// Path where the JSON-file client writes spans (when --protocol json+file).
    #[arg(long)]
    pub json_dir: Option<String>,

    /// Force a specific 16-byte trace id (32 hex chars).
    #[arg(long = "force-trace-id")]
    pub force_trace_id: Option<String>,

    /// Force a specific 8-byte span id (16 hex chars).
    #[arg(long = "force-span-id")]
    pub force_span_id: Option<String>,

    /// Force a specific 8-byte parent span id (16 hex chars).
    #[arg(long = "force-parent-span-id")]
    pub force_parent_span_id: Option<String>,

    /// Traceparent carrier file (read + optionally write).
    #[arg(long = "tp-carrier")]
    pub tp_carrier: Option<String>,

    /// Ignore the TRACEPARENT env var when loading.
    #[arg(long = "tp-ignore-env")]
    pub tp_ignore_env: bool,

    /// Print the resulting traceparent to stdout.
    #[arg(long = "tp-print")]
    pub tp_print: bool,

    /// Print the traceparent as `export TRACEPARENT=...`.
    #[arg(long = "tp-export")]
    pub tp_export: bool,

    /// Require a parseable traceparent in env or carrier.
    #[arg(long = "tp-required")]
    pub tp_required: bool,

    /// INTERNAL — when set, `span background` runs the IPC server directly
    /// instead of re-spawning itself. The foreground process uses this to
    /// detach the actual server. Hidden from --help.
    #[arg(long = "bg-mode-internal", hide = true)]
    pub bg_mode_internal: bool,

    #[command(subcommand)]
    pub command: Option<SpanCommand>,
}

#[derive(Debug, Subcommand)]
pub enum SpanCommand {
    /// Start a span and run a background server to add events/end the span later.
    Background(BackgroundArgs),
    /// Add an event to a running background span.
    Event(EventArgs),
    /// End a running background span.
    End(EndArgs),
}

#[derive(Debug, Args, Default)]
pub struct BackgroundArgs {
    /// Directory where the background server creates its IPC socket.
    #[arg(long)]
    pub sockdir: String,

    /// Wait for the background server to exit (instead of detaching).
    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args, Default)]
pub struct EventArgs {
    /// Background server sockdir.
    #[arg(long)]
    pub sockdir: String,

    /// Event name.
    #[arg(long, short = 'n')]
    pub name: String,

    /// Event time.
    #[arg(long, default_value = "now")]
    pub time: String,

    /// Event attributes as "k1=v1,k2=v2". Repeatable; see SpanArgs::attrs.
    #[arg(long, short = 'a', action = clap::ArgAction::Append)]
    pub attrs: Vec<String>,

    /// Ignore TRACEPARENT env (accepted for parity; event subcommand never
    /// re-emits traceparent itself).
    #[arg(long = "tp-ignore-env")]
    pub tp_ignore_env: bool,

    /// Print the running span's traceparent to stdout.
    #[arg(long = "tp-print")]
    pub tp_print: bool,
}

#[derive(Debug, Args, Default)]
pub struct EndArgs {
    /// Background server sockdir.
    #[arg(long)]
    pub sockdir: String,

    /// Optional explicit end time.
    #[arg(long, default_value = "now")]
    pub end: String,

    /// Extra attributes to merge into the final span. Repeatable; see
    /// SpanArgs::attrs.
    #[arg(long, short = 'a', action = clap::ArgAction::Append)]
    pub attrs: Vec<String>,

    /// Span status code override: unset | ok | error.
    #[arg(long = "status-code")]
    pub status_code: Option<String>,

    /// Span status description override.
    #[arg(long = "status-description")]
    pub status_description: Option<String>,

    /// Ignore TRACEPARENT env (accepted for parity).
    #[arg(long = "tp-ignore-env")]
    pub tp_ignore_env: bool,

    /// Print the running span's traceparent to stdout.
    #[arg(long = "tp-print")]
    pub tp_print: bool,
}

pub async fn run(args: SpanArgs) -> Result<u8> {
    // Dispatch to the background/event/end subcommands first.
    if let Some(sub) = &args.command {
        match sub {
            SpanCommand::Background(b) => return run_background(&args, b).await,
            SpanCommand::Event(e) => return run_event(e).await,
            SpanCommand::End(e) => return run_end(e).await,
        }
    }

    // 1. Assemble config: defaults → file → env → CLI overlay.
    let cfg = build_config(&args).context("building span config")?;

    // Update diagnostics for any downstream `status` view.
    if let Ok(mut diag) = diagnostics::global().lock() {
        diag.is_recording = cfg.is_recording();
        diag.endpoint = if !cfg.traces_endpoint.is_empty() {
            cfg.traces_endpoint.clone()
        } else {
            cfg.endpoint.clone()
        };
        diag.insecure_skip_verify = cfg.tls_no_verify;
    }

    // 2. Build the span (this also handles traceparent loading internally).
    let mut resource_spans = span_builder::build_resource_spans(&cfg);

    // 2a. If a memory-sampler is recording into OTEL_MEMORY_SAMPLES_FILE,
    // pick the in-window stats and merge them onto this span. Errors here
    // are non-fatal — a missing file (sampler not running) just yields no
    // attrs, which is the right behaviour for runs without instrumentation.
    merge_memory_window_attrs(&mut resource_spans);

    // 3. Instantiate client and send.
    let mut client = build_client(&cfg);
    let result: Result<()> = async {
        client.start().await.context("client start")?;
        client
            .upload_traces(vec![resource_spans.clone()])
            .await
            .context("upload traces")?;
        client.stop().await.context("client stop")?;
        Ok(())
    }
    .await;

    if let Err(ref err) = result {
        diagnostics::Diagnostics::set_error(&format!("{err:#}"));
        if cfg.fail {
            return Err(anyhow::anyhow!("{err:#}"));
        }
        // soft-fail: continue to propagate traceparent so downstream tools
        // still get a usable value
        eprintln!("otel-cli: warning: {err:#}");
    }

    // 4. Propagate traceparent: derive from the produced span (recording) or
    //    from the previously loaded TP (non-recording).
    propagate_traceparent(&cfg, &resource_spans).context("propagating traceparent")?;

    Ok(0)
}

/// Walk the precedence chain: defaults < file < env < CLI flags.
fn build_config(args: &SpanArgs) -> Result<Config> {
    let mut cfg = Config::defaults();

    // CLI config file flag takes effect immediately so envvars can still
    // override file-set fields. This mirrors Go's `LoadFile` then `LoadEnv`.
    if let Some(path) = args.common.config_file.as_deref() {
        if !path.is_empty() {
            cfg.load_file(path)
                .with_context(|| format!("loading config file {path:?}"))?;
        }
    }

    // Layer env on top — env-only fields populate from process env vars.
    cfg.load_env().context("loading env vars")?;

    // Finally the explicit CLI flags. clap default_values mean these are
    // always set, so we let the user's input win unconditionally.
    cfg.service_name = args.service.clone();
    if let Some(n) = &args.name {
        cfg.span_name = n.clone();
    }
    cfg.kind = args.kind.clone();
    cfg.span_start_time = args.start.clone();
    cfg.span_end_time = args.end.clone();
    cfg.status_code = args.status_code.clone();
    cfg.status_description = args.status_description.clone();

    // Merge every --attrs flag (each may itself be comma-separated). Later
    // flags overwrite earlier keys, matching OTEL_RESOURCE_ATTRIBUTES rules.
    for s in &args.attrs {
        let parsed = parse_attrs(s).map_err(|e| anyhow::anyhow!("--attrs: {e}"))?;
        cfg.attributes.extend(parsed);
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

    // Span-specific overrides
    if let Some(v) = &args.json_dir {
        cfg.json_dir = v.clone();
    }
    if let Some(v) = &args.force_trace_id {
        cfg.force_trace_id = v.clone();
    }
    if let Some(v) = &args.force_span_id {
        cfg.force_span_id = v.clone();
    }
    if let Some(v) = &args.force_parent_span_id {
        cfg.force_parent_span_id = v.clone();
    }
    if let Some(v) = &args.tp_carrier {
        cfg.traceparent_carrier_file = v.clone();
    }
    if args.tp_ignore_env {
        cfg.traceparent_ignore_env = true;
    }
    if args.tp_print {
        cfg.traceparent_print = true;
    }
    if args.tp_export {
        cfg.traceparent_print_export = true;
        // `--tp-export` implies `--tp-print` per the Go reference (it just
        // changes the print format).
        cfg.traceparent_print = true;
    }
    if args.tp_required {
        cfg.traceparent_required = true;
    }

    Ok(cfg)
}

/// If `OTEL_MEMORY_SAMPLES_FILE` points at a populated NDJSON samples file,
/// query the window `[span.start, span.end]` and merge `memory.rss.*` attrs
/// onto the span. Soft-fail on every error path: missing file, unparseable
/// rows, or out-of-window spans all just leave the span untouched. The
/// shell driver sets this env var unconditionally; absence of attrs is a
/// useful signal that the sampler didn't run.
fn merge_memory_window_attrs(rs: &mut ResourceSpans) {
    let path = match std::env::var("OTEL_MEMORY_SAMPLES_FILE") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };
    let Some(scope) = rs.scope_spans.get_mut(0) else { return };
    let Some(span) = scope.spans.get_mut(0) else { return };
    let stats = match memory::query_window(
        Path::new(&path),
        span.start_time_unix_nano,
        span.end_time_unix_nano,
    ) {
        Ok(Some(s)) => s,
        _ => return,
    };
    for (k, v) in stats.to_attrs() {
        span.attributes.push(KeyValue {
            key: k,
            value: Some(span_builder::string_to_any_value(&v)),
            ..Default::default()
        });
    }
}

/// Reconstruct a `Traceparent` from the just-built span and write/print it as
/// requested. Mirrors `config_span.go::PropagateTraceparent`.
fn propagate_traceparent(
    cfg: &Config,
    rs: &opentelemetry_proto::tonic::trace::v1::ResourceSpans,
) -> Result<()> {
    // We always print/save the *output* traceparent — the one for the span we
    // just emitted (recording) or the one loaded from input (non-recording).
    let tp = if cfg.is_recording() {
        let span = &rs.scope_spans[0].spans[0];
        let mut trace_id = [0u8; 16];
        let mut span_id = [0u8; 8];
        let tlen = trace_id.len().min(span.trace_id.len());
        trace_id[..tlen].copy_from_slice(&span.trace_id[..tlen]);
        let slen = span_id.len().min(span.span_id.len());
        span_id[..slen].copy_from_slice(&span.span_id[..slen]);
        Traceparent {
            version: 0,
            trace_id,
            span_id,
            flags: 0x01,
            initialized: true,
        }
    } else {
        // load whatever was already in env/file; return zero TP if missing
        match Traceparent::from_env() {
            Some(Ok(t)) => t,
            _ => {
                if !cfg.traceparent_carrier_file.is_empty() {
                    Traceparent::from_file(&cfg.traceparent_carrier_file).unwrap_or_default()
                } else {
                    Traceparent::default()
                }
            }
        }
    };

    if !cfg.traceparent_carrier_file.is_empty() {
        tp.write_to_file(&cfg.traceparent_carrier_file, cfg.traceparent_print_export)
            .with_context(|| {
                format!(
                    "writing traceparent to {:?}",
                    cfg.traceparent_carrier_file
                )
            })?;
    }

    if cfg.traceparent_print {
        let mut stdout = std::io::stdout().lock();
        if cfg.traceparent_print_export {
            writeln!(stdout, "# trace id: {}", tp.trace_id_string())?;
            writeln!(stdout, "#  span id: {}", tp.span_id_string())?;
            writeln!(stdout, "export TRACEPARENT={}", tp.encode())?;
        } else {
            writeln!(stdout, "# trace id: {}", tp.trace_id_string())?;
            writeln!(stdout, "#  span id: {}", tp.span_id_string())?;
            writeln!(stdout, "TRACEPARENT={}", tp.encode())?;
        }
    }

    Ok(())
}

// --------------------------------------------------------------------------
// Background mode dispatch
// --------------------------------------------------------------------------

#[cfg(unix)]
async fn run_background(args: &SpanArgs, b: &BackgroundArgs) -> Result<u8> {
    use std::path::PathBuf;

    if b.sockdir.is_empty() {
        anyhow::bail!("--sockdir is required for `span background`");
    }
    let sockdir = PathBuf::from(&b.sockdir);

    if args.bg_mode_internal {
        // We are the spawned child: run the server in-process.
        let mut cfg = build_config(args).context("building span config")?;
        cfg.background_sockdir = b.sockdir.clone();
        cfg.background_wait = b.wait;
        // Update diagnostics so a parallel `status` view can see us.
        if let Ok(mut diag) = diagnostics::global().lock() {
            diag.is_recording = cfg.is_recording();
            diag.endpoint = if !cfg.traces_endpoint.is_empty() {
                cfg.traces_endpoint.clone()
            } else {
                cfg.endpoint.clone()
            };
            diag.insecure_skip_verify = cfg.tls_no_verify;
        }
        crate::background::server::run_server(cfg, sockdir).await?;
        return Ok(0);
    }

    // Foreground path: spawn ourselves with --bg-mode-internal inserted
    // *before* the `background` subcommand (the flag lives on `SpanArgs`).
    let argv: Vec<String> = std::env::args().collect();
    let exe = std::env::current_exe().context("locating current exe")?;
    let original: Vec<String> = argv.iter().skip(1).cloned().collect();
    let mut child_args: Vec<String> = Vec::with_capacity(original.len() + 1);
    let mut inserted = false;
    for a in original {
        if !inserted && a == "background" {
            child_args.push("--bg-mode-internal".to_string());
            inserted = true;
        }
        child_args.push(a);
    }
    if !inserted {
        // Defensive: shouldn't happen because we only get here via the
        // `background` subcommand.
        child_args.push("--bg-mode-internal".to_string());
    }

    // Capture *our* parent (= the shell that invoked us). The detached child
    // will be reparented to init the moment we exit, so it cannot reliably
    // watch its own getppid(). Instead, it polls "is PID still alive".
    let watch_ppid = nix::unistd::getppid().as_raw();

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&child_args);
    cmd.env("OTEL_CLI_BG_WATCH_PPID", watch_ppid.to_string());
    // Detach stdio so the child outlives the parent's tty.
    use std::process::Stdio;
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    // Keep stderr so the server's "listening on" log can be seen during dev.
    cmd.stderr(Stdio::inherit());
    let mut child = cmd.spawn().context("spawning background server")?;

    if b.wait {
        // Wait until the child has bound the socket so callers can race-free
        // start firing event/end RPCs.
        let timeout = std::time::Duration::from_secs(10);
        crate::background::client::wait_for_socket(&sockdir, timeout)
            .await
            .context("waiting for background socket")?;
        // Ping with Wait so we know the accept loop is running.
        match crate::background::client::send_request(
            &sockdir,
            &crate::background::protocol::Request::Wait,
        )
        .await
        {
            Ok(_) => {}
            Err(e) => eprintln!("otel-cli: bg wait ping failed: {e:#}"),
        }
        // Then block on the child until it exits.
        let status = child.wait().await.context("waiting for bg child")?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
        }
    }
    // Detached mode: return immediately; the child runs on its own.
    Ok(0)
}

#[cfg(not(unix))]
async fn run_background(_args: &SpanArgs, _b: &BackgroundArgs) -> Result<u8> {
    anyhow::bail!("span background mode is not supported on Windows");
}

#[cfg(unix)]
async fn run_event(e: &EventArgs) -> Result<u8> {
    use crate::background::protocol::{Request, Response};
    use std::path::PathBuf;

    if e.sockdir.is_empty() {
        anyhow::bail!("--sockdir is required for `span event`");
    }
    let mut attrs: std::collections::BTreeMap<String, String> = Default::default();
    for s in &e.attrs {
        let parsed = parse_attrs(s).map_err(|m| anyhow::anyhow!("--attrs: {m}"))?;
        attrs.extend(parsed);
    }
    let req = Request::AddEvent {
        name: e.name.clone(),
        time: e.time.clone(),
        attrs,
    };
    let resp = crate::background::client::send_request(&PathBuf::from(&e.sockdir), &req).await?;
    match resp {
        Response::Ok { traceparent, .. } => {
            if e.tp_print {
                let mut stdout = std::io::stdout().lock();
                writeln!(stdout, "TRACEPARENT={traceparent}")?;
            }
            Ok(0)
        }
        Response::Err { message } => anyhow::bail!("span event: {message}"),
    }
}

#[cfg(not(unix))]
async fn run_event(_e: &EventArgs) -> Result<u8> {
    anyhow::bail!("span event mode is not supported on Windows");
}

#[cfg(unix)]
async fn run_end(e: &EndArgs) -> Result<u8> {
    use crate::background::protocol::{Request, Response};
    use std::path::PathBuf;

    if e.sockdir.is_empty() {
        anyhow::bail!("--sockdir is required for `span end`");
    }
    let mut attrs: std::collections::BTreeMap<String, String> = Default::default();
    for s in &e.attrs {
        let parsed = parse_attrs(s).map_err(|m| anyhow::anyhow!("--attrs: {m}"))?;
        attrs.extend(parsed);
    }
    let time = if e.end == "now" {
        None
    } else {
        Some(e.end.clone())
    };
    let req = Request::End {
        time,
        attrs,
        status_code: e.status_code.clone(),
        status_description: e.status_description.clone(),
    };
    let resp = crate::background::client::send_request(&PathBuf::from(&e.sockdir), &req).await?;
    match resp {
        Response::Ok { traceparent, .. } => {
            if e.tp_print {
                let mut stdout = std::io::stdout().lock();
                writeln!(stdout, "TRACEPARENT={traceparent}")?;
            }
            Ok(0)
        }
        Response::Err { message } => anyhow::bail!("span end: {message}"),
    }
}

#[cfg(not(unix))]
async fn run_end(_e: &EndArgs) -> Result<u8> {
    anyhow::bail!("span end mode is not supported on Windows");
}
