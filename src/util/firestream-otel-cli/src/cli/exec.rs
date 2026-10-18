//! `otel-cli exec` — run a child process inside a span.
//!
//! Go reference: `otelcli/exec.go`.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;
use opentelemetry_proto::tonic::trace::v1::status::StatusCode;
use opentelemetry_proto::tonic::trace::v1::Status;

use super::common::CommonArgs;
use super::exec_impl;
use crate::client::build_client;
use crate::config::{parse_attrs, Config};
use crate::diagnostics;
use crate::span as span_builder;
use crate::traceparent::Traceparent;

#[derive(Debug, Args)]
pub struct ExecArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Service name attribute.
    #[arg(long, short = 's', env = "OTEL_CLI_SERVICE_NAME", default_value = "otel-cli")]
    pub service: String,

    /// Span name.
    #[arg(long, short = 'n', env = "OTEL_CLI_SPAN_NAME")]
    pub name: Option<String>,

    /// Span kind.
    #[arg(long, short = 'k', env = "OTEL_CLI_TRACE_KIND", default_value = "client")]
    pub kind: String,

    /// Attributes as "k1=v1,k2=v2". Repeatable; each flag's value may itself
    /// be comma-separated. Matches OTEL_RESOURCE_ATTRIBUTES semantics.
    #[arg(long, short = 'a', action = clap::ArgAction::Append)]
    pub attrs: Vec<String>,

    /// Disable injection of TRACEPARENT and {{traceparent}} substitution.
    #[arg(long, env = "OTEL_CLI_EXEC_TP_DISABLE_INJECT")]
    pub exec_tp_disable_inject: bool,

    /// Timeout for the child command (e.g. "10s").
    #[arg(long, env = "OTEL_CLI_EXEC_CMD_TIMEOUT")]
    pub exec_command_timeout: Option<String>,

    /// Path where the JSON-file client writes spans (when --protocol json+file).
    #[arg(long)]
    pub json_dir: Option<String>,

    /// Force a specific 16-byte trace id (32 hex chars).
    #[arg(long = "force-trace-id")]
    pub force_trace_id: Option<String>,

    /// Force a specific 8-byte span id (16 hex chars).
    #[arg(long = "force-span-id")]
    pub force_span_id: Option<String>,

    /// Ignore the TRACEPARENT env var when loading.
    #[arg(long = "tp-ignore-env")]
    pub tp_ignore_env: bool,

    /// Print the resulting traceparent to stdout.
    #[arg(long = "tp-print")]
    pub tp_print: bool,

    /// Print the traceparent as `export TRACEPARENT=...`.
    #[arg(long = "tp-export")]
    pub tp_export: bool,

    /// Command and arguments. Anything after `--` is passed verbatim.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    pub command: Vec<String>,
}

pub async fn run(args: ExecArgs) -> Result<u8> {
    if args.command.is_empty() {
        let msg = "exec needs a command after --";
        diagnostics::Diagnostics::set_error(&msg);
        anyhow::bail!(msg);
    }

    // 1. Resolve config from defaults → file → env → CLI overlay. Mirrors the
    //    layered build used by `span::run`.
    let mut cfg = build_config(&args).context("building exec config")?;

    // 2. Default span name to the command line if the user didn't supply one
    //    (Go: exec.go ~line 85, also config_span.go::NewProtobufSpan).
    if args.name.is_none() {
        cfg.span_name = args.command.join(" ");
    }

    // Update diagnostics so `status` reflects this run.
    if let Ok(mut diag) = diagnostics::global().lock() {
        diag.is_recording = cfg.is_recording();
        diag.endpoint = if !cfg.traces_endpoint.is_empty() {
            cfg.traces_endpoint.clone()
        } else {
            cfg.endpoint.clone()
        };
        diag.insecure_skip_verify = cfg.tls_no_verify;
    }

    // 3. Build the span up-front — we need its trace/span ids for the
    //    traceparent we'll hand the child. Start/end will be overwritten with
    //    real timings once the child runs.
    let mut resource_spans = span_builder::build_resource_spans(&cfg);
    let (trace_id_bytes, span_id_bytes) = {
        let s = &resource_spans.scope_spans[0].spans[0];
        (s.trace_id.clone(), s.span_id.clone())
    };

    // 4. Encode the traceparent string for env-var/substitution use.
    let tp_str = {
        let mut trace_id = [0u8; 16];
        let mut span_id = [0u8; 8];
        let tlen = trace_id.len().min(trace_id_bytes.len());
        trace_id[..tlen].copy_from_slice(&trace_id_bytes[..tlen]);
        let slen = span_id.len().min(span_id_bytes.len());
        span_id[..slen].copy_from_slice(&span_id_bytes[..slen]);
        Traceparent {
            version: 0,
            trace_id,
            span_id,
            flags: 0x01,
            initialized: true,
        }
        .encode()
    };

    // 5. Build the child command. exec_impl handles {{traceparent}} subst and
    //    TRACEPARENT env injection (gated by exec_tp_disable_inject).
    let program = args.command[0].clone();
    let child_args: Vec<String> = args.command[1..].to_vec();
    let mut cmd = exec_impl::build_child_command(
        &program,
        &child_args,
        &tp_str,
        cfg.exec_tp_disable_inject,
    );

    // 6. Spawn + wait. We measure wall-clock from JUST before spawn to JUST
    //    after the child reaps so the span duration is the child's runtime.
    let start = Utc::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {program:?}"))?;

    let child_pid: u32 = child.id().unwrap_or(0);

    // Signal forwarding: catch SIGINT/SIGTERM and forward to the child instead
    // of letting them tear down the parent. We must still wait for the child
    // to die so we can record its exit code and send the span.
    let exit_status = wait_with_signal_forwarding(&mut child, child_pid).await?;

    let end = Utc::now();

    // 7. Compute child exit code. None happens on signal-only termination,
    //    which Unix encodes as 128 + signal — Go uses Go's ExitCode() == -1,
    //    we approximate with 128 (SIGNAL_BASE). Either way, the child died,
    //    so we report something.
    let exit_code: i32 = exit_status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = exit_status.signal() {
                return 128 + sig;
            }
        }
        1
    });

    if let Ok(mut diag) = diagnostics::global().lock() {
        diag.exec_exit_code = exit_code;
    }

    // 8. Patch the span: real times, real status, process attributes.
    {
        let span = &mut resource_spans.scope_spans[0].spans[0];
        span.start_time_unix_nano = start.timestamp_nanos_opt().unwrap_or(0).max(0) as u64;
        span.end_time_unix_nano = end.timestamp_nanos_opt().unwrap_or(0).max(0) as u64;

        if exit_code != 0 {
            span.status = Some(Status {
                code: StatusCode::Error as i32,
                message: format!("exit status {exit_code}"),
            });
        }

        let mut attrs = exec_impl::process_attributes(
            &program,
            &args.command,
            child_pid,
            std::process::id(),
        );
        span.attributes.append(&mut attrs);
    }

    // 9. Send the span via the configured client. Mirrors span.rs's lifecycle.
    let mut client = build_client(&cfg);
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
        diagnostics::Diagnostics::set_error(&format!("{err:#}"));
        if cfg.fail {
            return Err(anyhow::anyhow!("{err:#}"));
        }
        eprintln!("otel-cli: warning: {err:#}");
    }

    // 10. Propagate traceparent (print/export) — same logic as span.rs.
    propagate_traceparent(&cfg, &resource_spans).context("propagating traceparent")?;

    // 11. Use the child's exit code as our own. Cap at 255 since ExitCode is u8.
    Ok((exit_code & 0xff) as u8)
}

/// Wait on `child`, forwarding SIGINT/SIGTERM to it. The Unix path uses
/// dedicated signal streams; on Windows we settle for ctrl-c only.
async fn wait_with_signal_forwarding(
    child: &mut tokio::process::Child,
    child_pid: u32,
) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigint =
            signal(SignalKind::interrupt()).context("registering SIGINT handler")?;
        let mut sigterm =
            signal(SignalKind::terminate()).context("registering SIGTERM handler")?;

        loop {
            tokio::select! {
                status = child.wait() => {
                    return status.context("waiting for child");
                }
                _ = sigint.recv() => {
                    forward_signal(child_pid, nix::sys::signal::Signal::SIGINT);
                }
                _ = sigterm.recv() => {
                    forward_signal(child_pid, nix::sys::signal::Signal::SIGTERM);
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        // Best-effort on non-Unix: ctrl_c() and then keep waiting.
        loop {
            tokio::select! {
                status = child.wait() => {
                    return status.context("waiting for child");
                }
                _ = tokio::signal::ctrl_c() => {
                    let _ = child.start_kill();
                }
            }
        }
    }
}

#[cfg(unix)]
fn forward_signal(pid: u32, sig: nix::sys::signal::Signal) {
    if pid == 0 {
        return;
    }
    // Best-effort: if the child already died the kill() call will fail with
    // ESRCH which we simply ignore — the next loop iteration will reap it.
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), sig);
}

/// Walk the precedence chain: defaults < file < env < CLI flags.
/// Mirrors `span::build_config` but only sets the fields that `exec` cares
/// about. Phase 7 may want to extract this into a shared helper, but the
/// arg structs differ enough that duplication is cheaper than a generic.
fn build_config(args: &ExecArgs) -> Result<Config> {
    let mut cfg = Config::defaults();

    if let Some(path) = args.common.config_file.as_deref() {
        if !path.is_empty() {
            cfg.load_file(path)
                .with_context(|| format!("loading config file {path:?}"))?;
        }
    }

    cfg.load_env().context("loading env vars")?;

    cfg.service_name = args.service.clone();
    if let Some(n) = &args.name {
        cfg.span_name = n.clone();
    }
    cfg.kind = args.kind.clone();
    // start/end are filled in by run() after the child exits.

    for s in &args.attrs {
        let parsed = parse_attrs(s).map_err(|e| anyhow::anyhow!("--attrs: {e}"))?;
        cfg.attributes.extend(parsed);
    }

    if args.exec_tp_disable_inject {
        cfg.exec_tp_disable_inject = true;
    }
    if let Some(t) = &args.exec_command_timeout {
        cfg.exec_command_timeout = t.clone();
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

    // exec-specific overrides
    if let Some(v) = &args.json_dir {
        cfg.json_dir = v.clone();
    }
    if let Some(v) = &args.force_trace_id {
        cfg.force_trace_id = v.clone();
    }
    if let Some(v) = &args.force_span_id {
        cfg.force_span_id = v.clone();
    }
    if args.tp_ignore_env {
        cfg.traceparent_ignore_env = true;
    }
    if args.tp_print {
        cfg.traceparent_print = true;
    }
    if args.tp_export {
        cfg.traceparent_print_export = true;
        cfg.traceparent_print = true;
    }

    Ok(cfg)
}

/// Reconstruct a `Traceparent` from the emitted span and print/save it as
/// requested. Mirrors `span.rs::propagate_traceparent` but reduced to the
/// always-recording case (exec always builds a span).
fn propagate_traceparent(
    cfg: &Config,
    rs: &opentelemetry_proto::tonic::trace::v1::ResourceSpans,
) -> Result<()> {
    if !cfg.traceparent_print && cfg.traceparent_carrier_file.is_empty() {
        return Ok(());
    }

    let span = &rs.scope_spans[0].spans[0];
    let mut trace_id = [0u8; 16];
    let mut span_id = [0u8; 8];
    let tlen = trace_id.len().min(span.trace_id.len());
    trace_id[..tlen].copy_from_slice(&span.trace_id[..tlen]);
    let slen = span_id.len().min(span.span_id.len());
    span_id[..slen].copy_from_slice(&span.span_id[..slen]);
    let tp = Traceparent {
        version: 0,
        trace_id,
        span_id,
        flags: 0x01,
        initialized: true,
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
        use std::io::Write;
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
