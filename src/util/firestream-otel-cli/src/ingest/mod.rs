//! Build-tool output ingestion (PRD §9.4).
//!
//! Adapters that turn external build-tool telemetry into otel-cli spans. The
//! first (and currently only) ingestor is the Nix `internal-json` reader driven
//! by the `otel-cli nix-ingest` subcommand.
//!
//! Layering:
//! - [`nix_internal_json`] — the schema adapter. The *only* module that knows
//!   the Nix wire format; it emits the stable [`nix_internal_json::NixEvent`].
//! - [`drv_resolver`] — close-time enrichment via `nix derivation show` +
//!   `nix log`, with an in-process cache.
//! - [`cgroup`] — best-effort `memory.peak` probe.
//!
//! The span-lifecycle state machine that ties these together lives in
//! `cli::nix_ingest`.

pub mod cgroup;
pub mod drv_resolver;
pub mod nix_internal_json;
