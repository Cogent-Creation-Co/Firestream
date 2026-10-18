//! `otel-cli server` — embedded OTLP server (test/demo).
//!
//! Go references: `otelcli/server.go`, `server_tui.go`, `server_json.go`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use tokio::sync::mpsc;

use crate::server::{json::JsonSink, run_server, tui, SpanSink};

#[derive(Debug, Args)]
pub struct ServerArgs {
    /// Address to listen on for gRPC (default 0.0.0.0:4317). Empty disables gRPC.
    #[arg(long, default_value = "0.0.0.0:4317")]
    pub grpc_addr: String,

    /// Address to listen on for HTTP (default 0.0.0.0:4318). Empty disables HTTP.
    #[arg(long, default_value = "0.0.0.0:4318")]
    pub http_addr: String,

    /// Maximum number of spans to accept before exiting (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    pub max_spans: u32,

    #[command(subcommand)]
    pub mode: ServerMode,
}

#[derive(Debug, Subcommand)]
pub enum ServerMode {
    /// Render spans in a TUI table.
    Tui,
    /// Write each span as JSON to a directory.
    Json(JsonArgs),
}

#[derive(Debug, Args)]
pub struct JsonArgs {
    /// Output directory.
    #[arg(long)]
    pub dir: String,
}

fn parse_addr(s: &str, name: &str) -> Result<Option<SocketAddr>> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let addr: SocketAddr = trimmed
        .parse()
        .with_context(|| format!("invalid {name}: {trimmed:?}"))?;
    Ok(Some(addr))
}

pub async fn run(args: ServerArgs) -> Result<u8> {
    let grpc_addr = parse_addr(&args.grpc_addr, "--grpc-addr")?;
    let http_addr = parse_addr(&args.http_addr, "--http-addr")?;

    if grpc_addr.is_none() && http_addr.is_none() {
        anyhow::bail!("at least one of --grpc-addr or --http-addr must be set");
    }

    match args.mode {
        ServerMode::Json(j) => {
            tokio::fs::create_dir_all(&j.dir)
                .await
                .with_context(|| format!("create --dir {:?}", j.dir))?;

            // Startup replay (PRD §9.3): before entering the receive loop,
            // re-emit any orphaned background spans (open without close) from
            // the checkpoint log into this same json+file sink. A fresh CI
            // run's server thus recovers spans lost to a prior SIGKILL/OOM.
            // Shares the flock + scan + rename routine with `otel-cli replay`,
            // so the host-side daemon and this hook never double-emit.
            #[cfg(unix)]
            replay_orphans_at_startup(&j.dir).await;

            let sink = Arc::new(SpanSink::Json(JsonSink::new(j.dir)));
            run_server(grpc_addr, http_addr, args.max_spans, sink).await?;
        }
        ServerMode::Tui => {
            // bounded channel — backpressure to the receivers if the TUI lags
            let (tx, rx) = mpsc::channel(256);
            let sink = Arc::new(SpanSink::Tui(tx));

            let server_fut = run_server(grpc_addr, http_addr, args.max_spans, sink);
            let tui_fut = tui::run_tui(rx);

            // The TUI exits when the user presses 'q' or when its sender is
            // dropped (which happens when the server exits). The server exits
            // on max-spans or ctrl-c. Wait for both — whichever finishes
            // first signals the other to wind down.
            tokio::select! {
                res = server_fut => res?,
                res = tui_fut => res?,
            }
        }
    }
    Ok(0)
}

/// Replay orphaned checkpoint spans into the json+file directory `dir` at
/// server startup. No-op when `OTEL_CHECKPOINT_DIR` is unset. Failures are
/// logged, never fatal — a replay glitch must not stop the server booting.
#[cfg(unix)]
async fn replay_orphans_at_startup(dir: &str) {
    use std::time::Duration;

    let mut cfg = crate::config::Config::defaults();
    if cfg.load_env().is_err() {
        return;
    }
    let checkpoint_dir = match cfg.resolved_checkpoint_dir() {
        Some(d) => d,
        None => return,
    };
    // Emit recovered spans into the same json+file sink this server writes to.
    cfg.protocol = "json+file".to_string();
    cfg.json_dir = dir.to_string();

    // min_age = 0: the startup hook processes everything; only the host-side
    // timer daemon waits 10 min to avoid racing a live server.
    match crate::checkpoint::replay_dir(&checkpoint_dir, &cfg, Duration::ZERO).await {
        Ok(report) if report.orphans_emitted > 0 || report.files_processed > 0 => {
            eprintln!(
                "otel-cli server json: startup replay processed {} file(s), {} orphan span(s)",
                report.files_processed, report.orphans_emitted
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("otel-cli server json: startup replay failed: {e:#}"),
    }
}
