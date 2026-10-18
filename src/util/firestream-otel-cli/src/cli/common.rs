//! Shared argument groups.
//!
//! These mirror the persistent flags from `otelcli/root.go`. Each top-level
//! subcommand embeds [`CommonArgs`] via `#[command(flatten)]` so users get
//! identical flag surface area across commands.

use clap::Args;

/// OTLP transport flags shared by every command that emits spans.
#[derive(Debug, Clone, Args, Default)]
pub struct CommonArgs {
    /// OTLP endpoint (host:port for gRPC, http(s)://... for HTTP).
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT", global = true)]
    pub endpoint: Option<String>,

    /// OTLP traces-specific endpoint override.
    #[arg(long, env = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", global = true)]
    pub traces_endpoint: Option<String>,

    /// OTLP protocol: grpc | http/protobuf | http/json | json+file.
    #[arg(long, global = true)]
    pub protocol: Option<String>,

    /// Request timeout (e.g. "1s", "500ms").
    #[arg(long, global = true)]
    pub timeout: Option<String>,

    /// Custom OTLP headers as "k1=v1,k2=v2".
    #[arg(long = "otlp-headers", global = true)]
    pub otlp_headers: Option<String>,

    /// Allow plaintext (skip TLS) when the endpoint scheme is ambiguous.
    #[arg(long, global = true)]
    pub insecure: bool,

    /// Block until the OTLP transport is connected.
    #[arg(long = "otlp-blocking", global = true)]
    pub blocking: bool,

    /// Path to a JSON config file.
    #[arg(long = "config", short = 'c', global = true)]
    pub config_file: Option<String>,

    /// Verbose logging.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Exit non-zero on transport errors instead of swallowing them.
    #[arg(long, global = true)]
    pub fail: bool,

    /// Tee fanout mode (PRD §9.1). One of `otlp+file`, `otlp-only`, `file-only`.
    /// Default (off) preserves single-transport behaviour.
    #[arg(long, global = true)]
    pub tee: Option<String>,

    /// Directory for the file leg of the tee. Falls back to `--json-dir` when unset.
    #[arg(long = "tee-file-dir", global = true)]
    pub tee_file_dir: Option<String>,

    /// Issue fdatasync + parent-dir fsync on every write in the file leg.
    #[arg(long = "tee-durable", global = true)]
    pub tee_durable: bool,
}
