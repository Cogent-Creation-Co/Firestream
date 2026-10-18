//! Runtime resolution and loading of `ci-manifest.json`.
//!
//! Deliberately the same code shape as
//! `src/lib/rust/firestream-charts/src/reader.rs` — read a file, parse it into
//! the v1 [`Profile`] types, surface a typed error naming the path. The only
//! structural difference is that a CI profile is a single document, so there
//! is no index and no lazy per-entry cache.
//!
//! ## Resolution order
//!
//! Identical in spirit to how `firestream-charts` resolves its bundle
//! (`--charts-dir` → `FIRESTREAM_CHARTS_DIR` → `/opt/firestream/charts`):
//!
//! 1. `--profile <path>` (explicit CLI flag)
//! 2. `$FIRESTREAM_CI_PROFILE`
//! 3. `./ci-manifest.json`
//! 4. `/opt/firestream/ci/ci-manifest.json`
//!
//! Steps 2 and 3 accept either a *file* or a *directory*: `nix build
//! .#firestream-ci-profile` produces a store directory containing
//! `ci-manifest.json`, and the devshell points `FIRESTREAM_CI_PROFILE` at that
//! directory (exactly as it points `FIRESTREAM_CHARTS_DIR` at the chart farm).
//! Pointing at the file itself also works.

use std::path::{Path, PathBuf};

use super::spec::{Profile, ProfileError};

/// Canonical file name of the profile document.
pub const PROFILE_FILE: &str = "ci-manifest.json";

/// Env var carrying an explicit profile path (file or containing directory).
pub const PROFILE_ENV: &str = "FIRESTREAM_CI_PROFILE";

/// Default deploy location, mirroring `/opt/firestream/charts`.
pub const SYSTEM_PROFILE_DIR: &str = "/opt/firestream/ci";

/// If `p` is a directory, append [`PROFILE_FILE`]; otherwise use it verbatim.
fn as_profile_file(p: &Path) -> PathBuf {
    if p.is_dir() {
        p.join(PROFILE_FILE)
    } else {
        p.to_path_buf()
    }
}

/// The ordered candidate list, for both resolution and the not-found error
/// message. `explicit` short-circuits everything else: an operator who passed
/// `--profile` gets a hard error naming their path rather than a silent
/// fallthrough to some other repo's profile.
pub fn candidates(explicit: Option<&Path>) -> Vec<PathBuf> {
    if let Some(p) = explicit {
        return vec![as_profile_file(p)];
    }
    let mut out = Vec::new();
    if let Ok(v) = std::env::var(PROFILE_ENV) {
        if !v.trim().is_empty() {
            out.push(as_profile_file(Path::new(v.trim())));
        }
    }
    out.push(PathBuf::from(PROFILE_FILE));
    out.push(Path::new(SYSTEM_PROFILE_DIR).join(PROFILE_FILE));
    out
}

/// First existing candidate, or `None`.
pub fn resolve_path(explicit: Option<&Path>) -> Option<PathBuf> {
    candidates(explicit).into_iter().find(|p| p.is_file())
}

/// Parse and validate a profile document at `path`.
pub fn load(path: &Path) -> Result<Profile, ProfileError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ProfileError::NotFound(path.display().to_string())
        } else {
            ProfileError::Io {
                path: path.display().to_string(),
                source: e,
            }
        }
    })?;
    let profile: Profile = serde_json::from_str(&content).map_err(|e| ProfileError::Parse {
        path: path.display().to_string(),
        source: e,
    })?;
    profile.validate()?;
    Ok(profile)
}

/// Resolve, load and validate. Errors when nothing resolves — the caller
/// decides whether that is fatal (see [`resolve_or_default`]).
pub fn resolve(explicit: Option<&Path>) -> Result<Profile, ProfileError> {
    let cands = candidates(explicit);
    match cands.iter().find(|p| p.is_file()) {
        Some(p) => load(p),
        None => Err(ProfileError::NotFound(
            cands
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// [`resolve`], but a *missing* profile degrades to [`Profile::default`] with a
/// `tracing::debug!`. A profile that exists but is malformed still errors —
/// silently ignoring a broken profile is how a CI run ends up doing nothing at
/// all and calling it green.
///
/// This is what subcommands that merely *touch* profile data (`k8s namespace`,
/// `oci flatten`, `ci` dispatch) use, so `firestream-ci --help` and the
/// standalone utility subcommands keep working in a bare checkout.
pub fn resolve_or_default(explicit: Option<&Path>) -> Result<Profile, ProfileError> {
    match resolve(explicit) {
        Ok(p) => Ok(p),
        Err(ProfileError::NotFound(looked)) => {
            tracing::debug!(
                looked_at = %looked,
                "no ci-manifest.json resolved; using the built-in project-free default profile"
            );
            Ok(Profile::default())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_profile(dir: &Path, body: serde_json::Value) -> PathBuf {
        let p = dir.join(PROFILE_FILE);
        std::fs::write(&p, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
        p
    }

    #[test]
    fn loads_an_explicit_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_profile(
            tmp.path(),
            serde_json::json!({ "schema_version": 1, "project": { "name": "acme" } }),
        );
        let prof = load(&p).unwrap();
        assert_eq!(prof.project.name, "acme");
    }

    #[test]
    fn explicit_directory_resolves_to_the_file_inside() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), serde_json::json!({ "schema_version": 1 }));
        let prof = resolve(Some(tmp.path())).unwrap();
        assert_eq!(prof.schema_version, 1);
    }

    #[test]
    fn explicit_missing_path_does_not_fall_through() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        let cands = candidates(Some(&missing));
        assert_eq!(cands.len(), 1, "explicit path must short-circuit the chain");
        assert!(matches!(
            resolve(Some(&missing)),
            Err(ProfileError::NotFound(_))
        ));
    }

    #[test]
    fn malformed_profile_errors_rather_than_defaulting() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(PROFILE_FILE);
        std::fs::write(&p, b"{ not json").unwrap();
        assert!(matches!(
            resolve_or_default(Some(&p)),
            Err(ProfileError::Parse { .. })
        ));
    }

    #[test]
    fn missing_profile_degrades_to_default() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("absent.json");
        let prof = resolve_or_default(Some(&missing)).unwrap();
        assert_eq!(prof.schema_version, super::super::spec::SCHEMA_VERSION);
        assert!(prof.phases.is_empty());
    }

    #[test]
    fn candidate_chain_ends_at_the_system_path() {
        // No explicit path, env unset in this process by construction of the
        // assertion (we only check the tail, which is unconditional).
        let cands = candidates(None);
        let last = cands.last().unwrap();
        assert_eq!(last, &Path::new(SYSTEM_PROFILE_DIR).join(PROFILE_FILE));
        assert!(cands.iter().any(|p| p == Path::new(PROFILE_FILE)));
    }
}
