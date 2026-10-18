//! Native Nix build strategy
//!
//! This strategy builds containers using the host's native Nix installation.
//! It's the fastest option but only works on Linux systems with Nix installed.

use crate::config::BuildConfig;
use crate::discovery::ContainerInfo;
use crate::error::{NixContainerError, Result};
use crate::platform::PlatformInfo;
use crate::progress::{BuildPhase, BuildProgress};
use crate::strategy::{find_repo_root, BoxedProgressCallback, BuildMode, BuildStrategy, ContainerBuildStrategy};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Native Nix build strategy
///
/// Builds containers directly using the host's Nix installation.
/// This is only available on Linux systems.
pub struct NativeNixStrategy {
    platform: PlatformInfo,
}

impl NativeNixStrategy {
    /// Create a new native Nix strategy
    pub fn new(platform: PlatformInfo) -> Self {
        Self { platform }
    }

    /// Run the nix build command
    ///
    /// ## Why this spawns instead of calling `.output()`
    ///
    /// `.output()` buffers **both** streams until the child exits. A cold Spark
    /// or Airflow image is a 30+ minute derivation, and for all of it the user
    /// saw an entirely silent terminal. Here stderr is `inherit()`ed so Nix's
    /// progress (and, with `-L`, the full build logs of failing derivations)
    /// reaches the terminal live, while stdout stays `piped()` because that is
    /// where `--print-out-paths` writes the store path we must capture.
    ///
    /// Flags mirror `bin/build/strategy.sh`'s `fs_nix_build_native`:
    /// * `-L` / `--print-build-logs` — the Docker strategy already passes this.
    /// * `--no-update-lock-file` — a build must never silently mutate
    ///   `flake.lock`; if an input is missing, fail loudly instead.
    async fn run_nix_build(&self, flake_dir: &PathBuf, attr: &str) -> Result<PathBuf> {
        info!("Building with native Nix: {}#{}", flake_dir.display(), attr);

        let flake_ref = format!(".#{}", attr);

        let child = Command::new("nix")
            .args([
                "build",
                &flake_ref,
                "--no-link",
                "--print-out-paths",
                "-L",
                "--no-update-lock-file",
            ])
            .current_dir(flake_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        // stderr is inherited, so `output.stderr` is empty by construction; the
        // diagnostics the user needs have already been printed to the terminal.
        let output = child.wait_with_output().await?;

        if !output.status.success() {
            warn!("Nix build failed: nix build {} exited with {}", flake_ref, output.status);
            return Err(NixContainerError::BuildFailed {
                container: flake_dir.display().to_string(),
                message: format!(
                    "`nix build {}` failed with {} (build log was streamed above)",
                    flake_ref, output.status
                ),
            });
        }

        let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if path_str.is_empty() {
            return Err(NixContainerError::BuildFailed {
                container: flake_dir.display().to_string(),
                message: "Nix build produced no output".to_string(),
            });
        }

        // Take the first path if multiple are returned
        let path = path_str
            .lines()
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| NixContainerError::BuildFailed {
                container: flake_dir.display().to_string(),
                message: "Failed to parse Nix build output".to_string(),
            })?;

        debug!("Nix build output: {}", path.display());

        // Verify the output exists
        if !path.exists() {
            return Err(NixContainerError::BuildFailed {
                container: flake_dir.display().to_string(),
                message: format!("Build output does not exist: {}", path.display()),
            });
        }

        Ok(path)
    }
}

#[async_trait]
impl ContainerBuildStrategy for NativeNixStrategy {
    fn name(&self) -> &'static str {
        "Native Nix"
    }

    fn strategy_type(&self) -> BuildStrategy {
        BuildStrategy::NativeNix
    }

    async fn is_available(&self) -> bool {
        // Native Nix is only available on Linux with Nix installed
        self.platform.can_build_native()
    }

    async fn build(&self, container: &ContainerInfo, config: &BuildConfig) -> Result<PathBuf> {
        self.run_nix_build(&container.path, &config.nix_attribute).await
    }

    async fn build_with_progress(
        &self,
        container: &ContainerInfo,
        config: &BuildConfig,
        mut progress: BoxedProgressCallback,
    ) -> Result<PathBuf> {
        // Report resolving phase
        progress(BuildProgress::new(
            &container.name,
            BuildPhase::Resolving,
            "Resolving flake inputs...",
        ));

        // Report building phase
        progress(BuildProgress::new(
            &container.name,
            BuildPhase::Building { current: 1, total: 1 },
            format!("Building {}#{}...", container.path.display(), config.nix_attribute),
        ));

        // Run the actual build
        let result = self.run_nix_build(&container.path, &config.nix_attribute).await;

        // Report completion or failure
        match &result {
            Ok(path) => {
                progress(BuildProgress::complete(
                    &container.name,
                    format!("Built: {}", path.display()),
                ));
            }
            Err(e) => {
                progress(BuildProgress::failed(&container.name, e.to_string()));
            }
        }

        result
    }

    async fn build_with_mode(
        &self,
        mode: &BuildMode,
        config: &BuildConfig,
    ) -> Result<PathBuf> {
        match mode {
            BuildMode::RootFlake { package_name } => {
                let repo_root = find_repo_root()?;
                info!("Building with native Nix (root flake): .#{}", package_name);
                self.run_nix_build(&repo_root, package_name).await
            }
            BuildMode::SubdirFlake { container_dir, attribute } => {
                let container = ContainerInfo::new(
                    container_dir.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    container_dir,
                );
                let config = BuildConfig {
                    nix_attribute: attribute.clone(),
                    ..config.clone()
                };
                self.build(&container, &config).await
            }
        }
    }

    async fn build_with_mode_and_progress(
        &self,
        mode: &BuildMode,
        config: &BuildConfig,
        mut progress: BoxedProgressCallback,
    ) -> Result<PathBuf> {
        let label = match mode {
            BuildMode::RootFlake { package_name } => package_name.clone(),
            BuildMode::SubdirFlake { container_dir, .. } => container_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        };

        progress(BuildProgress::new(
            &label,
            BuildPhase::Resolving,
            "Resolving flake inputs...",
        ));

        progress(BuildProgress::new(
            &label,
            BuildPhase::Building { current: 1, total: 1 },
            format!("Building .#{}...", label),
        ));

        let result = self.build_with_mode(mode, config).await;

        match &result {
            Ok(path) => progress(BuildProgress::complete(&label, format!("Built: {}", path.display()))),
            Err(e) => progress(BuildProgress::failed(&label, e.to_string())),
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_strategy_name() {
        let platform = PlatformInfo {
            platform: crate::platform::Platform::Linux,
            arch: crate::platform::Architecture::X86_64,
            nix_available: true,
            docker_available: false,
            in_container: false,
        };
        let strategy = NativeNixStrategy::new(platform);
        assert_eq!(strategy.name(), "Native Nix");
        assert_eq!(strategy.strategy_type(), BuildStrategy::NativeNix);
    }

    #[test]
    fn test_build_mode_root_flake_variant_accepted() {
        // Verify RootFlake mode is a valid BuildMode variant
        // (the actual build would require Nix, but we verify the dispatch path compiles)
        let mode = BuildMode::RootFlake {
            package_name: "odoo-15".to_string(),
        };
        match &mode {
            BuildMode::RootFlake { package_name } => {
                assert_eq!(package_name, "odoo-15");
            }
            _ => panic!("Expected RootFlake"),
        }
    }

    #[test]
    fn test_build_mode_subdir_flake_variant_accepted() {
        let mode = BuildMode::SubdirFlake {
            container_dir: PathBuf::from("/tmp/test"),
            attribute: "dockerImage".to_string(),
        };
        match &mode {
            BuildMode::SubdirFlake { container_dir, attribute } => {
                assert_eq!(container_dir, &PathBuf::from("/tmp/test"));
                assert_eq!(attribute, "dockerImage");
            }
            _ => panic!("Expected SubdirFlake"),
        }
    }
}
