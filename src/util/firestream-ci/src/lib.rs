//! firestream-ci — build-pipeline primitives.
//!
//! A Nix + Docker + OTel build-orchestration toolkit composing the sibling
//! `otel-cli` and `firestream-nix-build` crates into a single library surface.
//! See `README.md` for the module map and the 22-pattern catalog this crate
//! is structured around, and `docs/oxi-ci-migration-scope.md` at the repo root
//! for how it got here.

pub mod config;
pub mod error;

pub mod artifacts;
pub mod checkpoint;
pub mod dashboard;
pub mod devshell;
pub mod exec;
pub mod imagebuild;
pub mod limits;
pub mod manifest;
pub mod nix;
pub mod oci;
pub mod passthrough;
pub mod pipeline;
pub mod platform;
pub mod profile;
pub mod reenter;
pub mod report;
pub mod rundir;
pub mod runner;
pub mod service;
pub mod trace;
pub mod util;
pub mod version;
pub mod wire;
pub mod worktree;

// NOTE: ConceptDB's `defaults` module (and the `conceptdb` cargo feature that
// gated it) were removed during the Firestream lift. Everything it encoded is
// now runtime data loaded by `crate::profile` from `ci-manifest.json` — the
// Nix-typed-options → JSON → Rust contract this repo already runs for Helm
// charts. The preserved inventory lives at `docs/defaults-reference.rs.txt`,
// and `tests/profile_fixtures.rs` reproduces its assertions from a synthetic
// non-Firestream profile, which is the gate on "no project-specific Rust".

// Top-level error re-export. Module-local errors are wired into
// `crate::error::Error` once the module bodies land in Phase 4+.
pub use error::Error;

// Most commonly used types from the plan's API design example.
pub use config::Config;
pub use pipeline::{Phase, Pipeline, Tier, Verdict};
pub use platform::{BuildStrategy, Decision, Probe};
pub use profile::Profile;
pub use trace::Tracer;
