//! Main builder API
//!
//! This module provides the main `NixContainerBuilder` struct that ties together
//! all the other components and provides the primary public API.

use crate::config::BuildConfig;
use crate::discovery::{ContainerDiscovery, ContainerInfo};
use crate::docker_loader::DockerLoader;
use crate::embedded::{containers_dir, extract_embedded, ExtractedWorkspace};
use crate::error::{NixContainerError, Result};
use crate::platform::PlatformInfo;
use crate::progress::{BuildPhase, BuildProgress, MultiProgress};
use crate::strategy::{create_strategy_with_workspace, BuildMode, BuildStrategy};
use futures::stream::{self, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Is this build artifact a path inside the (read-only) Nix store?
///
/// The native strategy hands back `--print-out-paths` output, which lives in
/// `/nix/store` and must never be unlinked: the store is read-only, so the
/// attempt fails, and on a store where it *would* succeed it would be
/// corruption. The Docker strategy hands back a genuine temp file.
fn is_nix_store_path(path: &Path) -> bool {
    path.starts_with("/nix/store")
}

/// Result of a successful container build
#[derive(Debug, Clone, serde::Serialize)]
pub struct BuildResult {
    /// Container name
    pub container: String,

    /// Full image name (repository)
    pub image_name: String,

    /// Image tag
    pub image_tag: String,

    /// Docker image ID (if available)
    pub image_id: Option<String>,

    /// Build duration
    #[serde(serialize_with = "serialize_duration")]
    pub duration: Duration,

    /// Build strategy used
    pub strategy: BuildStrategy,
}

fn serialize_duration<S: serde::Serializer>(duration: &Duration, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_f64(duration.as_secs_f64())
}

impl BuildResult {
    /// Get the full image reference
    pub fn full_ref(&self) -> String {
        format!("{}:{}", self.image_name, self.image_tag)
    }
}

impl std::fmt::Display for BuildResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} -> {}:{} ({:?}, {})",
            self.container,
            self.image_name,
            self.image_tag,
            self.duration,
            self.strategy
        )
    }
}

/// Main builder for Nix containers
///
/// This struct provides the primary API for building containers from Nix flakes.
/// It handles platform detection, strategy selection, and orchestrates the build process.
pub struct NixContainerBuilder {
    config: BuildConfig,
    platform: PlatformInfo,
    discovery: ContainerDiscovery,
    loader: DockerLoader,
    /// Workspace root for Docker strategy (used in portable/embedded mode)
    workspace_root: Option<PathBuf>,
    /// Extracted workspace (kept alive to prevent temp dir cleanup)
    #[allow(dead_code)]
    extracted_workspace: Option<ExtractedWorkspace>,
}

impl NixContainerBuilder {
    /// Create a new builder with default configuration
    ///
    /// This will detect the platform and use default paths.
    pub async fn new() -> Result<Self> {
        Self::with_config(BuildConfig::default()).await
    }

    /// Create a new builder with custom configuration
    pub async fn with_config(config: BuildConfig) -> Result<Self> {
        let platform = PlatformInfo::detect().await?;

        info!(
            "Platform detected: {} {} (Nix: {}, Docker: {}, Container: {})",
            platform.platform,
            platform.arch,
            platform.nix_available,
            platform.docker_available,
            platform.in_container
        );

        let discovery = ContainerDiscovery::new(&config.containers_dir);
        let loader = DockerLoader::new();

        Ok(Self {
            config,
            platform,
            discovery,
            loader,
            workspace_root: None,
            extracted_workspace: None,
        })
    }

    /// Create a new builder using embedded workspace (portable mode)
    ///
    /// This extracts the embedded Nix files to a temporary directory and
    /// uses that for building. The binary doesn't need to be run from within
    /// the repo, and doesn't require Nix on the host - only Docker.
    pub async fn embedded() -> Result<Self> {
        Self::embedded_with_config(BuildConfig::default()).await
    }

    /// Create a new builder using embedded workspace with custom configuration
    pub async fn embedded_with_config(config: BuildConfig) -> Result<Self> {
        let platform = PlatformInfo::detect().await?;

        info!(
            "Platform detected: {} {} (Nix: {}, Docker: {}, Container: {})",
            platform.platform,
            platform.arch,
            platform.nix_available,
            platform.docker_available,
            platform.in_container
        );

        // Extract embedded workspace using the new workspace-embed based function
        let workspace = extract_embedded()?;
        let workspace_root = workspace.root().to_path_buf();
        let workspace_containers_dir = containers_dir(&workspace);

        info!(
            "Using embedded workspace at: {}",
            workspace_root.display()
        );

        let discovery = ContainerDiscovery::new(&workspace_containers_dir);
        let loader = DockerLoader::new();

        Ok(Self {
            config,
            platform,
            discovery,
            loader,
            workspace_root: Some(workspace_root),
            extracted_workspace: Some(workspace),
        })
    }

    /// Get the current configuration
    pub fn config(&self) -> &BuildConfig {
        &self.config
    }

    /// Get platform information
    pub fn platform(&self) -> &PlatformInfo {
        &self.platform
    }

    /// Check if running in embedded/portable mode
    pub fn is_embedded(&self) -> bool {
        self.extracted_workspace.is_some()
    }

    /// Get the workspace root (if in embedded mode)
    pub fn workspace_root(&self) -> Option<&PathBuf> {
        self.workspace_root.as_ref()
    }

    /// Get the recommended build strategy
    pub fn recommended_strategy(&self) -> BuildStrategy {
        self.platform.recommended_strategy()
    }

    /// Discover all available containers
    pub async fn discover_containers(&self) -> Result<Vec<ContainerInfo>> {
        self.discovery.discover_nix().await
    }

    /// Get a specific container by name
    pub async fn get_container(&self, name: &str) -> Result<ContainerInfo> {
        self.discovery.get(name).await
    }

    /// Check if a container exists
    pub async fn container_exists(&self, name: &str) -> bool {
        self.discovery.exists(name).await
    }

    /// Build a single container by name
    pub async fn build(&self, name: &str) -> Result<BuildResult> {
        self.build_with_progress(name, |_| {}).await
    }

    /// Build a single container with progress reporting
    pub async fn build_with_progress<F>(
        &self,
        name: &str,
        mut progress: F,
    ) -> Result<BuildResult>
    where
        F: FnMut(BuildProgress) + Send,
    {
        let start = Instant::now();

        // Discover container
        progress(BuildProgress::discovering(name));
        let container = self.discovery.get(name).await?;

        if !container.has_flake {
            return Err(NixContainerError::FlakeNotFound {
                path: container.path.clone(),
            });
        }

        // Create build strategy (pass workspace root for embedded mode)
        let strategy = create_strategy_with_workspace(
            &self.platform,
            &self.config,
            self.workspace_root.clone(),
        );
        let strategy_type = strategy.strategy_type();

        info!("Building {} with {} strategy", name, strategy.name());

        // Check if strategy is available
        if !strategy.is_available().await {
            return Err(NixContainerError::UnsupportedPlatform(format!(
                "{} strategy is not available on this platform",
                strategy.name()
            )));
        }

        // Report build phase
        progress(BuildProgress::new(
            name,
            BuildPhase::Building { current: 1, total: 1 },
            format!("Building {}#{}...", container.path.display(), self.config.nix_attribute),
        ));

        // Build the container using the non-progress method
        let tarball_path = strategy
            .build(&container, &self.config)
            .await?;

        // Load into Docker
        progress(BuildProgress::loading(name));
        let load_result = self.loader.load_image(&tarball_path).await?;

        // Clean up the tarball.
        //
        // ONLY when it is ours to delete. The native Nix strategy returns a
        // `/nix/store/...` path straight out of `--print-out-paths`; the store
        // is read-only, so this unlink can only ever fail there (today a warn,
        // a hard error the moment anyone promotes it), and if it ever DID
        // succeed it would be corrupting the store. The Docker strategy writes
        // a real temp tarball, which is the case this cleanup exists for.
        // `build_package_with_progress` omits the cleanup entirely.
        if !is_nix_store_path(&tarball_path) {
            if let Err(e) = std::fs::remove_file(&tarball_path) {
                warn!("Failed to clean up tarball: {}", e);
            }
        } else {
            debug!(
                "Skipping cleanup of read-only Nix store path: {}",
                tarball_path.display()
            );
        }

        let duration = start.elapsed();

        progress(BuildProgress::complete(
            name,
            format!("Built {} in {:?}", load_result.full_ref(), duration),
        ));

        Ok(BuildResult {
            container: name.to_string(),
            image_name: load_result.image_name,
            image_tag: load_result.image_tag,
            image_id: load_result.image_id,
            duration,
            strategy: strategy_type,
        })
    }

    /// Build multiple containers sequentially
    pub async fn build_all(&self, names: &[&str]) -> Result<Vec<BuildResult>> {
        self.build_all_with_progress(names, |_| {}).await
    }

    /// Build multiple containers sequentially with progress reporting
    pub async fn build_all_with_progress<F>(
        &self,
        names: &[&str],
        mut progress: F,
    ) -> Result<Vec<BuildResult>>
    where
        F: FnMut(BuildProgress) + Send,
    {
        let mut results = Vec::new();

        for (i, name) in names.iter().enumerate() {
            progress(BuildProgress::new(
                *name,
                BuildPhase::Building {
                    current: i + 1,
                    total: names.len(),
                },
                format!("Building container {} of {}", i + 1, names.len()),
            ));

            let result = self.build(name).await?;
            results.push(result);
        }

        Ok(results)
    }

    /// Build multiple containers in parallel
    ///
    /// # Arguments
    /// * `names` - Container names to build
    /// * `max_concurrent` - Maximum number of concurrent builds (None = use config default)
    pub async fn build_parallel(
        &self,
        names: &[&str],
        max_concurrent: Option<usize>,
    ) -> Result<Vec<BuildResult>> {
        self.build_parallel_with_progress(names, max_concurrent, |_| {})
            .await
    }

    /// Build multiple containers in parallel with progress reporting
    pub async fn build_parallel_with_progress<F>(
        &self,
        names: &[&str],
        max_concurrent: Option<usize>,
        progress: F,
    ) -> Result<Vec<BuildResult>>
    where
        F: FnMut(BuildProgress) + Send + 'static,
    {
        let max_concurrent = max_concurrent.unwrap_or(self.config.max_concurrent);
        let progress = Arc::new(Mutex::new(progress));
        let multi_progress = Arc::new(Mutex::new(MultiProgress::new(names.len())));

        info!(
            "Building {} containers with {} concurrent jobs",
            names.len(),
            max_concurrent
        );

        // Convert names to owned strings for the async closures
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();

        let results: Vec<Result<BuildResult>> = stream::iter(names)
            .map(|name| {
                let progress = Arc::clone(&progress);
                let multi_progress = Arc::clone(&multi_progress);
                async move {
                    // Update multi-progress
                    {
                        let mut mp = multi_progress.lock().await;
                        mp.start(&name);
                    }

                    // Notify progress
                    {
                        let mut p = progress.lock().await;
                        p(BuildProgress::new(
                            &name,
                            BuildPhase::Building { current: 0, total: 0 },
                            "Starting build...",
                        ));
                    }

                    // Build the container
                    let result = self.build(&name).await;

                    // Update multi-progress
                    {
                        let mut mp = multi_progress.lock().await;
                        match &result {
                            Ok(_) => mp.complete_success(&name),
                            Err(_) => mp.complete_failed(&name),
                        }
                    }

                    // Notify progress
                    {
                        let mut p = progress.lock().await;
                        match &result {
                            Ok(r) => p(BuildProgress::complete(&name, format!("Built {}", r.full_ref()))),
                            Err(e) => p(BuildProgress::failed(&name, e.to_string())),
                        }
                    }

                    result
                }
            })
            .buffer_unordered(max_concurrent)
            .collect()
            .await;

        // Separate successes and failures
        let mut successes = Vec::new();
        let mut errors = Vec::new();

        for result in results {
            match result {
                Ok(build_result) => successes.push(build_result),
                Err(e) => errors.push(e),
            }
        }

        // If any builds failed, return the first error
        if let Some(first_error) = errors.into_iter().next() {
            warn!(
                "{} builds succeeded, {} failed",
                successes.len(),
                1 // We're only returning the first error
            );
            return Err(first_error);
        }

        Ok(successes)
    }

    /// Build a container by root flake package name (e.g., "airflow", "postgresql-17").
    ///
    /// This uses `BuildMode::RootFlake` to build from the repo root with `nix build .#<package>`.
    /// This is the preferred method for all containers, especially those without per-container flake.nix
    /// (airflow, jupyterhub, odoo, superset).
    pub async fn build_package(&self, package_name: &str) -> Result<BuildResult> {
        self.build_package_with_progress(package_name, |_| {}).await
    }

    /// Build a container by root flake package name with progress reporting.
    pub async fn build_package_with_progress<F>(
        &self,
        package_name: &str,
        mut progress: F,
    ) -> Result<BuildResult>
    where
        F: FnMut(BuildProgress) + Send,
    {
        let start = Instant::now();

        progress(BuildProgress::new(
            package_name,
            BuildPhase::Resolving,
            format!("Resolving package .#{}...", package_name),
        ));

        // Create build strategy
        let strategy = create_strategy_with_workspace(
            &self.platform,
            &self.config,
            self.workspace_root.clone(),
        );
        let strategy_type = strategy.strategy_type();

        info!("Building package {} with {} strategy", package_name, strategy.name());

        if !strategy.is_available().await {
            return Err(NixContainerError::UnsupportedPlatform(format!(
                "{} strategy is not available on this platform",
                strategy.name()
            )));
        }

        let mode = BuildMode::RootFlake {
            package_name: package_name.to_string(),
        };

        progress(BuildProgress::new(
            package_name,
            BuildPhase::Building { current: 1, total: 1 },
            format!("Building .#{}...", package_name),
        ));

        // Build using the root flake mode
        let tarball_path = strategy.build_with_mode(&mode, &self.config).await?;

        // Load into Docker
        progress(BuildProgress::loading(package_name));
        let load_result = self.loader.load_image(&tarball_path).await?;

        let duration = start.elapsed();

        progress(BuildProgress::complete(
            package_name,
            format!("Built {} in {:?}", load_result.full_ref(), duration),
        ));

        Ok(BuildResult {
            container: package_name.to_string(),
            image_name: load_result.image_name,
            image_tag: load_result.image_tag,
            image_id: load_result.image_id,
            duration,
            strategy: strategy_type,
        })
    }

    /// Build all discovered containers
    pub async fn build_all_discovered(&self) -> Result<Vec<BuildResult>> {
        let containers = self.discover_containers().await?;
        let names: Vec<&str> = containers.iter().map(|c| c.name.as_str()).collect();
        self.build_all(&names).await
    }

    /// Build all discovered containers in parallel
    pub async fn build_all_discovered_parallel(
        &self,
        max_concurrent: Option<usize>,
    ) -> Result<Vec<BuildResult>> {
        let containers = self.discover_containers().await?;
        let names: Vec<&str> = containers.iter().map(|c| c.name.as_str()).collect();
        self.build_parallel(&names, max_concurrent).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_result_display() {
        let result = BuildResult {
            container: "redis".to_string(),
            image_name: "firestream-redis".to_string(),
            image_tag: "7.0".to_string(),
            image_id: Some("sha256:abc123".to_string()),
            duration: Duration::from_secs(60),
            strategy: BuildStrategy::NativeNix,
        };

        let display = result.to_string();
        assert!(display.contains("redis"));
        assert!(display.contains("firestream-redis:7.0"));
        assert!(display.contains("Native Nix"));
    }
}
