//! Builder-pattern runtime configuration.
//!
//! Three fields: where the repo is, where per-run output goes, and **the
//! loaded [`Profile`]**. The profile is the whole point — it is the runtime
//! payload that replaced ConceptDB's compiled-in `defaults` module (see
//! `crate::profile`), so `Config` is the single place a caller assembles
//! "everything this process needs to know that isn't in its argv".
//!
//! [`ConfigBuilder::profile_path`] takes the `--profile` flag verbatim and
//! runs it through the standard resolution chain (`--profile` →
//! `FIRESTREAM_CI_PROFILE` → `./ci-manifest.json` →
//! `/opt/firestream/ci/ci-manifest.json`); [`ConfigBuilder::profile`] injects
//! an already-loaded one, which is how the agent-mode `RunRequest.profile`
//! inline payload and the fixture tests get in.

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::profile::Profile;

#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) repo_root: PathBuf,
    pub(crate) build_output_dir: PathBuf,
    pub(crate) profile: Profile,
    /// Path the profile was actually loaded from, when it came from disk.
    /// `None` when injected directly or when the built-in default was used.
    pub(crate) profile_path: Option<PathBuf>,
}

impl Config {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn build_output_dir(&self) -> &Path {
        &self.build_output_dir
    }

    /// The loaded CI profile. Always present — a missing `ci-manifest.json`
    /// degrades to [`Profile::default`], which is deliberately project-free.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Where the profile came from, for diagnostics / the run banner.
    pub fn profile_path(&self) -> Option<&Path> {
        self.profile_path.as_deref()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ConfigBuilder {
    repo_root: Option<PathBuf>,
    build_output_dir: Option<PathBuf>,
    profile: Option<Profile>,
    profile_path: Option<PathBuf>,
}

impl ConfigBuilder {
    pub fn repo_root(mut self, path: impl AsRef<Path>) -> Self {
        self.repo_root = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn build_output_dir(mut self, path: impl AsRef<Path>) -> Self {
        self.build_output_dir = Some(path.as_ref().to_path_buf());
        self
    }

    /// Explicit `--profile <path>`. Resolution still runs (a directory is
    /// accepted and `ci-manifest.json` appended), but the chain stops here:
    /// an explicit path that does not exist is an error, never a fallthrough.
    pub fn profile_path(mut self, path: impl AsRef<Path>) -> Self {
        self.profile_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Inject an already-loaded profile, bypassing disk entirely. Used by the
    /// agent-mode inline `RunRequest.profile` payload and by tests.
    pub fn profile(mut self, profile: Profile) -> Self {
        self.profile = Some(profile);
        self
    }

    pub fn build(self) -> Result<Config, Error> {
        let repo_root = self
            .repo_root
            .ok_or_else(|| Error::Other("Config: repo_root is required".to_string()))?;
        let build_output_dir = self
            .build_output_dir
            .ok_or_else(|| Error::Other("Config: build_output_dir is required".to_string()))?;

        let (profile, profile_path) = match self.profile {
            Some(p) => {
                p.validate()?;
                (p, None)
            }
            None => {
                let explicit = self.profile_path.as_deref();
                let path = crate::profile::resolve_path(explicit);
                let p = crate::profile::resolve_or_default(explicit)?;
                (p, path)
            }
        };

        Ok(Config {
            repo_root,
            build_output_dir,
            profile,
            profile_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_profile_wins_over_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = Profile::default();
        p.project.name = "acme".into();
        let cfg = Config::builder()
            .repo_root(tmp.path())
            .build_output_dir(tmp.path())
            .profile(p)
            .build()
            .unwrap();
        assert_eq!(cfg.profile().project.name, "acme");
        assert!(cfg.profile_path().is_none());
    }

    #[test]
    fn explicit_profile_path_is_loaded() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ci-manifest.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "project": { "name": "acme", "nix_system": "x86_64-linux", "arch": "x86_64" }
            }))
            .unwrap(),
        )
        .unwrap();
        let cfg = Config::builder()
            .repo_root(tmp.path())
            .build_output_dir(tmp.path())
            .profile_path(&path)
            .build()
            .unwrap();
        assert_eq!(cfg.profile().project.name, "acme");
        assert_eq!(cfg.profile_path(), Some(path.as_path()));
    }
}
