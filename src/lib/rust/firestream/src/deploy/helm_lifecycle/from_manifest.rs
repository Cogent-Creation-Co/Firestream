//! Conversion from `firestream_charts::ChartManifest` to `ChartInfo`.
//!
//! Phase 5 of the typed Nix→JSON→Rust contract: the `firestream-charts` crate
//! (Agent D) deliberately stopped at parsing JSON manifests so it would not
//! create a dependency cycle with the top-level `firestream` crate. This
//! module owns the conversion from those manifests to the in-crate
//! `ChartInfo` type that drives the helm lifecycle executor.
//!
//! It also exposes a process-wide `Charts` registry initialized lazily from
//! the `FIRESTREAM_CHARTS_DIR` environment variable, so chart files
//! (`charts/airflow.rs`, `charts/postgresql.rs`, ...) can request their
//! manifest at construction time without each rebuilding the reader.
//!
//! The conversion does NOT touch fields the manifest doesn't carry yet
//! (breaking-version semantics, generated yaml, helm `--set` overrides).
//! Those are intentionally left at `ChartInfo::default()` values; chart
//! files that need to layer extra `values` entries on top of the
//! manifest-derived base mutate `ChartInfo.values` directly.

use std::path::PathBuf;
use std::sync::OnceLock;

use firestream_charts::{ChartManifest, Charts};

use super::types::{ChartInfo, HelmChartNamespace};

/// Default location for the chart bundle when `FIRESTREAM_CHARTS_DIR` is
/// unset. Matches the symlink target that `flake.nix`'s devShell creates and
/// the `/opt/firestream/charts` mount point used inside containers.
pub const DEFAULT_CHARTS_DIR: &str = "/opt/firestream/charts";

/// Environment variable that overrides the default chart bundle location.
pub const CHARTS_DIR_ENV: &str = "FIRESTREAM_CHARTS_DIR";

static CHARTS: OnceLock<Result<Charts, firestream_charts::Error>> = OnceLock::new();

/// Resolve the chart bundle directory from the environment.
///
/// Order of preference:
/// 1. `$FIRESTREAM_CHARTS_DIR` if set and non-empty.
/// 2. `DEFAULT_CHARTS_DIR` (`/opt/firestream/charts`) — the in-container path.
pub fn charts_dir() -> PathBuf {
    match std::env::var(CHARTS_DIR_ENV) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => PathBuf::from(DEFAULT_CHARTS_DIR),
    }
}

/// Process-wide chart registry. Lazily initialized on first call.
///
/// Returns `None` if the bundle directory has no readable `index.json`. This
/// is a deliberate soft failure: chart files that want the manifest fall back
/// to their hand-rolled `ChartInfo::new(...)` baseline when the registry is
/// absent (e.g. running outside the dev shell, or before the Nix build
/// completes).
pub fn try_registry() -> Option<&'static Charts> {
    let result = CHARTS.get_or_init(|| Charts::open(charts_dir()));
    result.as_ref().ok()
}

/// Convert a parsed `ChartManifest` into the helm-lifecycle `ChartInfo`.
///
/// Field-by-field rationale:
///
/// - `name`, `chart`, `version` are direct copies.
/// - `path = Some(manifest.bundle.chart_path)` — chart source lives in the
///   Nix-built bundle, not a remote helm repo.
/// - `repository = None` — the executor falls through to `path` when set.
/// - `values_files = [manifest.bundle.values_path]` — this is the "values
///   seam" the Nix side fills in; chart files may extend this vec with extra
///   files but should not replace the base entry.
/// - `namespace = HelmChartNamespace::Default`, `custom_namespace = Some(name)`
///   — per Agent D's note, the existing `HelmChartNamespace` enum's `Custom`
///   variant carries a `u32` index, NOT a String, so we cannot project the
///   manifest's namespace string through it. Routing through
///   `custom_namespace` matches how `get_namespace()` / the executor
///   resolves namespaces in practice.
/// - `atomic`, `wait`, `wait_for_jobs`, `timeout`, `force_upgrade`,
///   `hooks_disabled`, `skip_crds` are copied verbatim from the manifest's
///   `deployment` block.
/// - `create_namespace` comes from the manifest's `release` block.
/// - `depends_on` comes from `lifecycle.depends_on`.
/// - `last_breaking_version_requiring_restart = None` — the spec crate's
///   `BreakingVersion` (loose split-semver shape) does not map cleanly onto
///   `ChartInfo::BreakingVersion` (which carries restart commands). Until
///   the Nix side emits restart metadata, leave this `None` and let chart
///   files that need breaking-change handling override after conversion.
/// - `values`, `yaml_files_content` start empty. Chart files layer Rust-only
///   `--set` overrides on top after calling this function.
pub fn chart_info_from_manifest(manifest: &ChartManifest) -> ChartInfo {
    let namespace_str = manifest
        .release
        .namespace
        .clone()
        .filter(|s| !s.is_empty());

    let mut values_files = Vec::new();
    let values_path = &manifest.bundle.values_path;
    if values_path.as_os_str().is_empty() {
        // No values file emitted by Nix — leave the vec empty so
        // `check_prerequisites` does not error trying to stat a bogus path.
    } else {
        values_files.push(values_path.clone());
    }

    ChartInfo {
        name: manifest.name.clone(),
        repository: None,
        chart: manifest.chart.clone(),
        version: Some(manifest.version.clone()),
        path: Some(manifest.bundle.chart_path.clone()),
        namespace: HelmChartNamespace::Default,
        custom_namespace: namespace_str,
        action: super::types::HelmAction::Deploy,
        atomic: manifest.deployment.atomic,
        force_upgrade: manifest.deployment.force_upgrade,
        create_namespace: manifest.release.create_namespace,
        last_breaking_version_requiring_restart: None,
        timeout: manifest.deployment.timeout.clone(),
        dry_run: false,
        wait: manifest.deployment.wait,
        wait_for_jobs: manifest.deployment.wait_for_jobs,
        values: Vec::new(),
        values_files,
        yaml_files_content: Vec::new(),
        depends_on: manifest.lifecycle.depends_on.clone(),
        hooks_disabled: manifest.deployment.hooks_disabled,
        skip_crds: manifest.deployment.skip_crds,
    }
}

/// Convenience: look up `name` in the registry and convert to `ChartInfo`.
///
/// Returns `None` when the registry isn't available (no bundle on disk) or
/// the chart isn't registered. Callers should treat this as "fall back to
/// hand-rolled defaults" rather than as a hard error — the same chart files
/// must remain usable when the Nix-built bundle isn't present (developer
/// running `cargo test` without `nix build`, etc.).
pub fn chart_info_for(name: &str) -> Option<ChartInfo> {
    let charts = try_registry()?;
    match charts.get(name) {
        Ok(m) => Some(chart_info_from_manifest(&m)),
        Err(e) => {
            tracing::debug!(
                chart = name,
                error = %e,
                "chart manifest lookup failed; falling back to hand-rolled ChartInfo"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_minimal_manifest_routes_namespace_through_custom() {
        let mut m = ChartManifest {
            schema_version: "1".into(),
            name: "airflow".into(),
            chart: "airflow".into(),
            version: "1.2.3".into(),
            release: Default::default(),
            bundle: Default::default(),
            deployment: Default::default(),
            lifecycle: Default::default(),
            images: Default::default(),
            provenance: Default::default(),
            backup: None,
        };
        m.release.namespace = Some("data".into());
        m.release.create_namespace = true;
        m.bundle.chart_path = PathBuf::from("/nix/store/abc-airflow/airflow");
        m.bundle.values_path = PathBuf::from("/nix/store/abc-airflow/values.yaml");
        m.deployment.timeout = "600s".into();
        m.deployment.wait = true;

        let info = chart_info_from_manifest(&m);
        assert_eq!(info.name, "airflow");
        assert_eq!(info.chart, "airflow");
        assert_eq!(info.version.as_deref(), Some("1.2.3"));
        assert_eq!(info.path.as_deref().and_then(|p| p.to_str()),
                   Some("/nix/store/abc-airflow/airflow"));
        assert!(info.repository.is_none());
        assert_eq!(info.custom_namespace.as_deref(), Some("data"));
        assert!(info.create_namespace);
        assert_eq!(info.timeout, "600s");
        assert!(info.wait);
        assert_eq!(info.values_files.len(), 1);
    }

    #[test]
    fn empty_values_path_yields_empty_values_files() {
        let m = ChartManifest {
            schema_version: "1".into(),
            name: "x".into(),
            chart: "x".into(),
            version: "0.0.0".into(),
            release: Default::default(),
            bundle: Default::default(),
            deployment: Default::default(),
            lifecycle: Default::default(),
            images: Default::default(),
            provenance: Default::default(),
            backup: None,
        };
        let info = chart_info_from_manifest(&m);
        assert!(info.values_files.is_empty());
    }
}
