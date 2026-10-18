//! v1 schema types for chart-manifest.json and index.json.
//!
//! These types mirror the JSON shape emitted by `bin/nix/firestream/charts/eval-chart.nix`
//! (per-chart manifest) and the aggregate `index.json` written by Phase 3
//! (see `nix/flake-modules/charts/`).
//!
//! ## Shape rules (per Agent A + Agent C hand-off)
//!
//! - JSON keys are emitted in camelCase by Nix (`pkgs.formats.json`), so all
//!   structs use `#[serde(rename_all = "camelCase")]`.
//! - Null leaves are STRIPPED by Nix's emitter. Every optional field carries
//!   `#[serde(default, skip_serializing_if = "Option::is_none")]` so missing
//!   keys parse cleanly and round-trip without re-emitting nulls.
//! - Every collection (`Vec`, `BTreeMap`) carries `#[serde(default)]` so a
//!   missing key (e.g. no `images`) becomes an empty collection rather than
//!   a parse error.
//! - JSON attribute order is alphabetized by `pkgs.formats.json`; parsing is
//!   key-based, never positional.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A single chart's full manifest, parsed from `<chart>/chart-manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartManifest {
    /// Schema version of this manifest. Currently `"1"`.
    pub schema_version: String,

    /// Logical Firestream name of the chart (the key in the index).
    pub name: String,

    /// Helm chart name (often equal to `name`, but may differ when vendoring
    /// a Bitnami chart under a different Firestream slot).
    pub chart: String,

    /// Helm chart version (`Chart.yaml: version`).
    pub version: String,

    /// Release / install-time parameters (namespace, release name).
    #[serde(default)]
    pub release: Release,

    /// Pointers into the Nix-built chart bundle (chart path, rendered yaml,
    /// values yaml).
    #[serde(default)]
    pub bundle: Bundle,

    /// Helm deploy-time flags (atomic, wait, timeout, ...).
    #[serde(default)]
    pub deployment: Deployment,

    /// Cross-chart lifecycle metadata (dependencies, breaking-version
    /// markers).
    #[serde(default)]
    pub lifecycle: Lifecycle,

    /// Image overrides keyed by Firestream image slot. Each slot points at
    /// a sub-path of the chart's values file via `componentPath`.
    #[serde(default)]
    pub images: BTreeMap<String, ImageSlot>,

    /// Build provenance (flake revision, nixpkgs revision). May be empty.
    #[serde(default)]
    pub provenance: Provenance,

    /// Backup/restore contract. Absent when the chart ships no backup
    /// CronJob (`_meta.backup = null` on the Nix side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<Backup>,
}

/// How `firestream helm backup|restore` finds and drives a chart's backup
/// CronJob.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    /// Suffix appended to the Bitnami fullname to name the CronJob, e.g.
    /// `pgdumpall` -> `<fullname>-pgdumpall`.
    pub cron_job_suffix: String,

    /// Scale the `<fullname>` Deployment to zero before the restore Job and
    /// back to its previous replica count afterwards.
    #[serde(default)]
    pub quiesce_deployment: bool,
}

/// Helm release / namespace placement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// Target Kubernetes namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    /// Helm release name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,

    /// Whether the deployer should `helm install --create-namespace`.
    #[serde(default)]
    pub create_namespace: bool,
}

/// Pointers into the Nix-built chart bundle.
///
/// All paths are absolute store paths emitted by Nix. The reader does not
/// validate that they exist at parse time — the deployer is responsible for
/// reading them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    /// Path to the unpacked Helm chart directory.
    #[serde(default)]
    pub chart_path: PathBuf,

    /// Path to the Nix-rendered baseline `values.yaml`.
    #[serde(default)]
    pub values_path: PathBuf,

    /// Path to the pre-rendered Kubernetes manifest YAML (output of
    /// `helm template`).
    #[serde(default)]
    pub rendered_path: PathBuf,
}

/// Helm install/upgrade flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    /// Pass `--atomic` to `helm upgrade --install`.
    #[serde(default)]
    pub atomic: bool,

    /// Pass `--wait`.
    #[serde(default)]
    pub wait: bool,

    /// Pass `--wait-for-jobs`.
    #[serde(default)]
    pub wait_for_jobs: bool,

    /// `--timeout` value (e.g. `"10m"`, `"300s"`). Kept as a string so the
    /// Nix-side representation (Helm duration grammar) round-trips losslessly.
    #[serde(default = "default_timeout")]
    pub timeout: String,

    /// Pass `--force` to `helm upgrade`.
    #[serde(default)]
    pub force_upgrade: bool,

    /// Pass `--no-hooks`.
    #[serde(default)]
    pub hooks_disabled: bool,

    /// Pass `--skip-crds`.
    #[serde(default)]
    pub skip_crds: bool,
}

impl Default for Deployment {
    fn default() -> Self {
        Self {
            atomic: false,
            wait: false,
            wait_for_jobs: false,
            timeout: default_timeout(),
            force_upgrade: false,
            hooks_disabled: false,
            skip_crds: false,
        }
    }
}

fn default_timeout() -> String {
    "300s".to_string()
}

/// Cross-chart lifecycle metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lifecycle {
    /// Other charts (by Firestream name) that must be deployed before this
    /// one.
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// Marker for the last chart version whose schema requires a destroy +
    /// re-create instead of a regular upgrade. Absent (null) for charts with
    /// no breaking history yet (e.g. airflow today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_breaking_version: Option<BreakingVersion>,
}

/// Semver marker for the last breaking version of a chart.
///
/// The exact shape of this type isn't fully locked yet — Agent A left it as
/// `null` for airflow. Kept loose (all fields optional strings) so that
/// whichever shape the Nix side eventually emits will deserialize without
/// requiring a schema bump.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakingVersion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub major: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minor: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,

    /// Free-form version string (e.g. `"24.0.0"`). Useful when the Nix side
    /// emits a flat string instead of split semver components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A single image override slot.
///
/// `component_path` is the dotted path INTO the chart's values file where the
/// image lives — for the top-level Bitnami pattern this is just `["image"]`,
/// but subcharts use deeper paths like `["postgresql", "image"]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSlot {
    /// Path into values.yaml that this slot targets. Empty = root.
    #[serde(default)]
    pub component_path: Vec<String>,

    /// Optional registry override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,

    /// Image repository (e.g. `"firestream-airflow"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// Image tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Build provenance. Both fields may be absent when the build wasn't run
/// against a clean flake (e.g. local `nix build` with dirty git state).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake_revision: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nixpkgs_revision: Option<String>,
}

/// Aggregate index over all built charts plus named stacks.
///
/// Parsed from `index.json` at the root of the charts symlink farm.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    /// Schema version (currently `"1"`).
    pub schema_version: String,

    /// Charts keyed by Firestream name.
    #[serde(default)]
    pub charts: BTreeMap<String, ChartIndexEntry>,

    /// Base (un-overlaid) charts keyed by Firestream name. Added in Phase 5;
    /// each value points at the base chart directory (Chart.yaml at root, no
    /// Firestream values overlay / no image injection). Optional and additive:
    /// an older index without this key parses to an empty map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub base_charts: BTreeMap<String, BaseChartEntry>,

    /// Named stacks (deployment groups), each value is an ordered list of
    /// chart names. Entries MAY reference charts not present in `charts` —
    /// the reader handles that gracefully (see `Charts::stack`).
    #[serde(default)]
    pub stacks: BTreeMap<String, Vec<String>>,
}

/// One entry in `Index.charts`. Holds the relative path to that chart's
/// `chart-manifest.json` (resolved against the index's parent directory at
/// read time).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartIndexEntry {
    /// Path RELATIVE to the index's parent directory.
    pub manifest_path: PathBuf,
}

/// One entry in `Index.base_charts`. Holds the relative path to that chart's
/// base (un-overlaid) chart directory (resolved against the index's parent
/// directory at read time). The directory has `Chart.yaml` at its root.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseChartEntry {
    /// Path RELATIVE to the index's parent directory (e.g. `postgresql-base`).
    pub base_chart_path: PathBuf,
}

/// Convenience alias for a named stack (an ordered list of chart names).
pub type Stack = Vec<String>;
