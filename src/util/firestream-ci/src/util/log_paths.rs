//! Canonical filename helpers for per-attr `nix-fast-build` outputs.
//!
//! Used by both the writer (in `bin/firestream-ci.rs`, `build_nix_attr_task`) and
//! the dashboard's failure-tail reader (`dashboard/mod.rs`). Centralising
//! the path shape here closes the silent-advisory-miss bug: before, the
//! writer used `{phase}-{attr_leaf}.stderr.log` while the reader rebuilt
//! `{phase}-{tier}-{task}.stderr.log` and coincidentally lined up only for
//! `required-*` (because the task name had `required-` stripped). Advisory
//! tasks doubled their `advisory-` segment, the file was never found, and
//! the failure-tail block silently rendered empty.
//!
//! There is no per-tier subdirectory; tier is already encoded in the
//! `attr_leaf` (`required-rust-fmt`, `advisory-rust-audit`). `tier` is
//! retained in `stderr_log_path`'s signature for caller documentation and
//! to make the call site explicit about which classification it intends —
//! the helper itself does not consult it.

use std::path::{Path, PathBuf};

use crate::pipeline::Tier;

/// Per-attr stderr log path: `{logs}/{phase}-{attr_leaf}.stderr.log`.
///
/// `attr_leaf` is the dotted last segment of the full nix-attr (e.g.
/// `required-rust-fmt`, `advisory-rust-audit`); it already carries the
/// tier as a prefix. `tier` is accepted for symmetry with the manifest
/// and dashboard call sites but intentionally not consulted — encoding
/// it again in the filename would either be redundant (for leaves that
/// already start with the tier word) or actively wrong (the
/// double-`advisory-` regression we are fixing).
pub fn stderr_log_path(logs: &Path, phase: &str, tier: Tier, attr_leaf: &str) -> PathBuf {
    let _ = tier;
    logs.join(format!("{phase}-{attr_leaf}.stderr.log"))
}

/// Per-attr `nix-fast-build` phase log path.
///
/// Was a *shared* file before this refactor — every attr in a phase
/// wrote to `nix-fast-build-{phase}.log`, last-writer-wins. Now keyed
/// by `safe_attr` (dots/slashes replaced with underscores) so 8+
/// concurrent attrs each get their own file.
pub fn nix_fast_build_log_path(logs: &Path, phase: &str, safe_attr: &str) -> PathBuf {
    logs.join(format!("nix-fast-build-{phase}.{safe_attr}.log"))
}

/// Per-attr `nix-fast-build` result JSON path.
pub fn nix_fast_build_result_path(logs: &Path, phase: &str, safe_attr: &str) -> PathBuf {
    logs.join(format!("nix-fast-build-{phase}.{safe_attr}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_log_path_is_deterministic_and_tier_agnostic_for_shape() {
        let logs = Path::new("/r/logs");
        // Same shape regardless of tier — the leaf carries the tier prefix.
        let a = stderr_log_path(logs, "verify", Tier::Required, "required-rust-fmt");
        let b = stderr_log_path(logs, "verify", Tier::Advisory, "advisory-rust-audit");
        assert_eq!(a, Path::new("/r/logs/verify-required-rust-fmt.stderr.log"));
        assert_eq!(
            b,
            Path::new("/r/logs/verify-advisory-rust-audit.stderr.log")
        );
        // No double-`advisory-` regression.
        assert!(!b.to_string_lossy().contains("advisory-advisory"));
    }

    #[test]
    fn nix_fast_build_paths_are_per_attr() {
        let logs = Path::new("/r/logs");
        let l1 = nix_fast_build_log_path(logs, "build", "required-server");
        let l2 = nix_fast_build_log_path(logs, "build", "required-web");
        assert_ne!(l1, l2, "per-attr paths must not collide");
        assert_eq!(
            l1,
            Path::new("/r/logs/nix-fast-build-build.required-server.log")
        );
        let j = nix_fast_build_result_path(logs, "build", "required-server");
        assert_eq!(
            j,
            Path::new("/r/logs/nix-fast-build-build.required-server.json")
        );
    }
}
