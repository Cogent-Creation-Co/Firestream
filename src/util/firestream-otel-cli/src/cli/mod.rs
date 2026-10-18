//! Command-line interface — mirrors otel-cli's cobra command tree.
//!
//! Go reference: `otelcli/root.go`.

use anyhow::Result;
use clap::{Parser, Subcommand};

pub mod common;
pub mod completion;
pub mod exec;
pub mod exec_impl;
pub mod memory_sampler;
pub mod nix_ingest;
pub mod reconcile;
pub mod replay;
pub mod server;
pub mod span;
pub mod status;
pub mod version;

#[derive(Debug, Parser)]
#[command(
    name = "otel-cli",
    about = "OpenTelemetry CLI — send spans from shell scripts",
    version,
    propagate_version = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

// Box the larger variants to keep clippy's `large_enum_variant` happy.
// SpanArgs/ExecArgs aggregate dozens of clap flags via `flatten` and can
// easily exceed 500 bytes — sized this way the enum stays compact.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create and send a span.
    Span(Box<span::SpanArgs>),
    /// Execute a child process inside a span.
    Exec(Box<exec::ExecArgs>),
    /// Send canary spans for diagnostics.
    Status(status::StatusArgs),
    /// Run an embedded OTLP server.
    Server(server::ServerArgs),
    /// Re-emit orphaned background spans from the checkpoint log.
    Replay(replay::ReplayArgs),
    /// Read Nix internal-json from stdin and emit per-derivation build spans.
    NixIngest(nix_ingest::NixIngestArgs),
    /// Sample a process tree's aggregate RSS at a fixed interval, writing
    /// NDJSON samples for `otel-cli span` to attach as `memory.rss.*`.
    MemorySampler(memory_sampler::MemorySamplerArgs),
    /// Override per-derivation span status from a nix-fast-build result file.
    ReconcileSpans(reconcile::ReconcileArgs),
    /// Generate shell completion scripts.
    Completion(completion::CompletionArgs),
    /// Print version information.
    Version,
}

pub async fn dispatch() -> Result<u8> {
    let cli = Cli::parse();
    match cli.command {
        Command::Span(args) => span::run(*args).await,
        Command::Exec(args) => exec::run(*args).await,
        Command::Status(args) => status::run(args).await,
        Command::Server(args) => server::run(args).await,
        Command::Replay(args) => replay::run(args).await,
        Command::NixIngest(args) => nix_ingest::run(args).await,
        Command::MemorySampler(args) => memory_sampler::run(args).await,
        Command::ReconcileSpans(args) => reconcile::run(args).await,
        Command::Completion(args) => completion::run(args),
        Command::Version => version::run(),
    }
}
