//! OTLP client implementations.
//!
//! Go reference: `otlpclient/`. The [`OtlpClient`] trait mirrors the Go
//! `OTLPClient` interface (`otlp_client.go:22-26`).

use async_trait::async_trait;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use thiserror::Error;

pub mod grpc;
pub mod http_common;
pub mod http_json;
pub mod http_proto;
pub mod json_file;
pub mod null;
pub mod retry;
pub mod tee;

use crate::config::{Config, ConfigError, Protocol, TeeMode};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("OTLP returned non-success status: {0}")]
    Status(String),
    #[error("marshal error: {0}")]
    Marshal(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait OtlpClient: Send {
    async fn start(&mut self) -> Result<(), ClientError>;
    async fn upload_traces(
        &mut self,
        spans: Vec<ResourceSpans>,
    ) -> Result<(), ClientError>;
    async fn stop(&mut self) -> Result<(), ClientError>;
}

/// Resolve a transport and return the corresponding boxed client. Mirrors
/// `otlpclient/otlp_client.go::StartClient` — except start() is left to the
/// caller so the factory itself doesn't need to be `async`.
///
/// The tee-mode selector (PRD §9.1) takes precedence: when `OTEL_CLI_TEE`
/// is set, this returns a [`tee::TeeClient`] composing the configured
/// network transport with a [`json_file::JsonFileClient`].
pub fn build_client(cfg: &Config) -> Box<dyn OtlpClient> {
    match cfg.resolved_tee_mode() {
        TeeMode::Off => build_single(cfg),
        TeeMode::OtlpOnly => build_network(cfg),
        TeeMode::FileOnly => build_file(cfg),
        TeeMode::OtlpAndFile => {
            let otlp = build_network(cfg);
            let file = build_file(cfg);
            Box::new(tee::TeeClient::new(otlp, file))
        }
    }
}

/// Single-transport selection — the pre-tee behaviour.
fn build_single(cfg: &Config) -> Box<dyn OtlpClient> {
    match cfg.resolved_protocol() {
        Protocol::Null => Box::new(null::NullClient),
        Protocol::JsonFile(dir) => {
            Box::new(json_file::JsonFileClient::with_durable(dir, cfg.tee_durable))
        }
        Protocol::Grpc => Box::new(grpc::GrpcClient::new(cfg)),
        Protocol::HttpProto => Box::new(http_proto::HttpProtoClient::new(cfg)),
        Protocol::HttpJson => Box::new(http_json::HttpJsonClient::new(cfg)),
    }
}

/// Build only the network leg. Skips JsonFile (it would never be the network
/// leg of a tee) and falls back to Null when no endpoint is configured.
fn build_network(cfg: &Config) -> Box<dyn OtlpClient> {
    match cfg.resolved_protocol() {
        Protocol::Grpc => Box::new(grpc::GrpcClient::new(cfg)),
        Protocol::HttpProto => Box::new(http_proto::HttpProtoClient::new(cfg)),
        Protocol::HttpJson => Box::new(http_json::HttpJsonClient::new(cfg)),
        // When the resolved protocol picks JsonFile or Null for the network
        // slot, there isn't a real network endpoint; emit nothing rather than
        // double-writing to the file side.
        Protocol::JsonFile(_) | Protocol::Null => Box::new(null::NullClient),
    }
}

/// Build only the file leg. Honours `tee_durable` so the file copy survives
/// process crashes.
fn build_file(cfg: &Config) -> Box<dyn OtlpClient> {
    let dir = cfg.resolved_tee_file_dir();
    Box::new(json_file::JsonFileClient::with_durable(dir, cfg.tee_durable))
}

/// Construct a client from the ambient `OTEL_*` environment.
///
/// Convenience wrapper used by sibling crates (e.g. `firestream-ci`) that don't carry
/// their own [`Config`] plumbing. Mirrors the env-overlay used by every CLI
/// subcommand: start from defaults, overlay [`Config::load_env`], hand to
/// [`build_client`]. The boxed client is unstarted — call `start()` before the
/// first `upload_traces`.
///
/// Surfaces [`ConfigError`] when `load_env` rejects a malformed bool or map
/// env var. On a clean default environment this always returns a usable
/// client (a [`null::NullClient`] when no endpoint is set).
pub fn client_from_env() -> Result<Box<dyn OtlpClient>, ConfigError> {
    let mut cfg = Config::defaults();
    cfg.load_env()?;
    Ok(build_client(&cfg))
}
