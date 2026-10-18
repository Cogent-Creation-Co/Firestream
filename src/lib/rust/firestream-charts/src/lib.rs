//! Firestream Helm chart manifest schema + runtime reader.
//!
//! This crate is the Rust consumer side of the typed Nix → JSON → Rust contract
//! used by the Firestream Helm deployment pipeline. The Nix side (see
//! `bin/nix/firestream/charts/`) emits a per-chart `chart-manifest.json` plus an
//! aggregate `index.json` at the root of a symlink farm. This crate parses
//! both, with a small in-memory cache for per-chart manifests.
//!
//! No `build.rs`. No directory scanning. No `nix eval`. The reader takes a
//! `charts_dir` path at construction time and reads JSON files from disk.
//!
//! # Modules
//!
//! - [`spec`] — `#[derive(Deserialize, Serialize)]` types matching the v1
//!   manifest + index schemas emitted by Nix (see
//!   `bin/nix/firestream/charts/eval-chart.nix` for the producer).
//! - [`reader`] — [`Charts`] runtime reader: loads `index.json` eagerly and
//!   per-chart manifests lazily on first access.
//!
//! # Conversion to `firestream::deploy::helm_lifecycle::ChartInfo`
//!
//! A `ChartManifest → ChartInfo` conversion is intentionally NOT implemented
//! here. Doing so would require depending on the top-level `firestream` crate
//! and create a dependency cycle (the Phase 5 plan has `firestream` depending
//! on `firestream-charts`). Agent F (Phase 5) owns adding `impl From<&ChartManifest>
//! for ChartInfo` inside the `firestream` crate.
//!
//! # Example
//!
//! ```no_run
//! use firestream_charts::Charts;
//!
//! let charts = Charts::open("/nix/store/...-firestream-charts").unwrap();
//! let airflow = charts.get("airflow").unwrap();
//! println!("airflow chart path: {}", airflow.bundle.chart_path.display());
//!
//! for chart in charts.stack("dev").unwrap() {
//!     println!("deploying {} v{}", chart.name, chart.version);
//! }
//! ```

pub mod reader;
pub mod spec;

pub use reader::Charts;
pub use spec::{
    Backup, BaseChartEntry, BreakingVersion, Bundle, ChartIndexEntry, ChartManifest, Deployment,
    ImageSlot, Index, Lifecycle, Provenance, Release,
};

/// Error type for chart manifest operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `index.json` was missing at the given charts directory.
    #[error("Charts index not found at {0}")]
    IndexNotFound(String),

    /// A `chart-manifest.json` referenced by the index was missing on disk.
    #[error("Chart manifest not found at {0}")]
    ManifestNotFound(String),

    /// JSON could not be parsed against the v1 schema.
    #[error("Failed to parse {file}: {source}")]
    ParseError {
        file: String,
        #[source]
        source: serde_json::Error,
    },

    /// Caller asked for a chart that isn't listed in `index.json`.
    #[error("Chart not registered in index: {0}")]
    ChartNotInIndex(String),

    /// Caller asked for a stack that isn't listed in `index.json`.
    #[error("Stack not registered in index: {0}")]
    StackNotInIndex(String),

    /// Generic IO failure reading manifest files.
    #[error("IO error reading {file}: {source}")]
    Io {
        file: String,
        #[source]
        source: std::io::Error,
    },

    /// Cache lock poisoned (should not happen in practice).
    #[error("Internal cache lock poisoned")]
    LockPoisoned,
}
