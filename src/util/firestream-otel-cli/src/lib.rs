//! otel-cli — Rust port of equinix-labs/otel-cli.

pub mod checkpoint;
pub mod cli;
pub mod client;
pub mod config;
pub mod diagnostics;
pub mod ingest;
pub mod json_layout;
pub mod memory;
pub mod server;
pub mod span;
pub mod traceparent;

#[cfg(unix)]
pub mod background;

pub use diagnostics::Diagnostics;

// Re-exports pinned by `tests/lib_api_surface.rs` — the public surface the
// sibling `firestream-ci` crate consumes during the Phase 2 toolkit extraction.
// Keep this list narrow; each name here is exercised by the surface test.
pub use cli::reconcile::{reconcile_spans, ReconcileError, ReconcileReport};
pub use client::grpc::GrpcClient;
pub use client::http_json::HttpJsonClient;
pub use client::http_proto::HttpProtoClient;
pub use client::json_file::JsonFileClient;
pub use client::null::NullClient;
pub use client::{build_client, client_from_env, OtlpClient};
pub use config::{Config, ConfigError};

use anyhow::Result;

pub fn version() -> String {
    format!(
        "{} {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    )
}

pub async fn run() -> Result<u8> {
    cli::dispatch().await
}
