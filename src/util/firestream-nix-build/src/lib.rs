//! firestream-nix-build — Rust port of Mic92/nix-fast-build with in-process
//! OpenTelemetry ingest (PRD §9.2).

pub mod build;
pub mod ci_summary;
pub mod cli;
pub mod display;
pub mod eval;
pub mod nix_config;
pub mod nom;
pub mod options;
pub mod otel;
pub mod queue;
pub mod remote;
pub mod result;
pub mod ring;
pub mod run;
pub mod stop;
pub mod upload;

pub use display::{RingFormatter, strip_ansi};
pub use options::{EvalMode, Options, ResultFormat};
pub use result::{Outcome, ResultKind};
pub use ring::LineRing;
