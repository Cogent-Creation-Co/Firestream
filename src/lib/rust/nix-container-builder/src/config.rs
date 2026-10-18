//! Configuration types for nix-container-builder

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Main configuration for building containers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    /// Path to containers directory (default: src/containers/firestream)
    pub containers_dir: PathBuf,

    /// Nix attribute to build (default: dockerImage)
    pub nix_attribute: String,

    /// Force native Nix build even if Docker available
    pub force_native: bool,

    /// Force Docker-based build even on Linux
    pub force_docker: bool,

    /// Named volume for Nix store persistence
    pub nix_store_volume: Option<String>,

    /// Build timeout in seconds
    #[serde(with = "duration_serde")]
    pub timeout: Duration,

    /// Retry configuration
    pub retries: RetryConfig,

    /// Maximum concurrent builds (for parallel building)
    pub max_concurrent: usize,

    /// Package registry mapping container names to root flake package names.
    /// Mirrors the CONTAINER_REGISTRY from bin/build/_common.sh.
    /// Keys are "container" or "container:version", values are flake package names.
    #[serde(default = "default_package_registry")]
    pub package_registry: HashMap<String, String>,
}

/// Create the default package registry matching `bin/build/registry-cases.json`.
///
/// # This is a MIRROR, not an authority
///
/// The authority is `bin/build/registry-cases.json` (`entries`), which
/// `bin/build/test-registry-parity.sh` already gates against
/// `bin/build/_common.sh`'s `CONTAINER_REGISTRY` and, via
/// `bin/nix/firestream/ci/profile.nix` -> `ci.containerRegistry`, against
/// `firestream_ci::profile::Profile::resolve_package_name`.
///
/// This copy exists because `nix-container-builder` is a **root workspace**
/// member and `firestream-ci` lives in the deliberately isolated `src/util`
/// workspace (edition 2024, own lockfile). Taking a cargo dependency on it to
/// read one table would drag ~15 duplicate transitive majors into the root
/// lock. Instead the drift is gated by `tests/registry_parity.rs`, which
/// `include_str!`s the same JSON file and asserts this table equals `entries`
/// key-for-key in both directions.
///
/// Keys here use the bare container name for "the family default" where the
/// bash array uses a trailing colon (`airflow` <-> `airflow:`); the parity test
/// performs that translation explicitly.
///
/// **Do not hand-edit this list.** Edit `bin/build/registry-cases.json` and let
/// the parity test tell you what to change here.
fn default_package_registry() -> HashMap<String, String> {
    let entries = [
        // PostgreSQL
        ("postgresql", "postgresql-17"),
        ("postgresql:16", "postgresql-16"),
        ("postgresql:17", "postgresql-17"),
        // Redis — THE LANDMINE: `.#redis` is redis-8 in the flake, but a bare
        // `redis` on the build path is redis-7. See registry-cases.json.
        ("redis", "redis-7"),
        ("redis:7", "redis-7"),
        ("redis:8", "redis-8"),
        // Superset
        ("superset", "superset-5"),
        ("superset:4", "superset-4"),
        ("superset:5", "superset-5"),
        // Airflow
        ("airflow", "airflow-3"),
        ("airflow:3", "airflow-3"),
        // Kafka
        ("kafka", "kafka-4"),
        ("kafka:4", "kafka-4"),
        // Spark
        ("spark", "spark-4"),
        ("spark:4", "spark-4"),
        // JupyterHub
        ("jupyterhub", "jupyterhub-5"),
        ("jupyterhub:5", "jupyterhub-5"),
        // Odoo — bare `odoo` resolves to the UNSUFFIXED package, unlike every
        // other family. Preserved bug-for-bug from CONTAINER_REGISTRY.
        ("odoo", "odoo"),
        ("odoo:15", "odoo-15"),
        ("odoo:16", "odoo-16"),
        ("odoo:17", "odoo-17"),
        ("odoo:18", "odoo-18"),
    ];
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            containers_dir: PathBuf::from("src/containers/firestream"),
            nix_attribute: "dockerImage".to_string(),
            force_native: false,
            force_docker: false,
            nix_store_volume: None,
            timeout: Duration::from_secs(600),
            retries: RetryConfig::default(),
            max_concurrent: num_cpus::get(),
            package_registry: default_package_registry(),
        }
    }
}

impl BuildConfig {
    /// Create a new BuildConfig with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a BuildConfig optimized for embedded/portable mode
    ///
    /// This configuration forces Docker-based builds since the portable binary
    /// cannot rely on native Nix being installed on the host.
    pub fn for_embedded() -> Self {
        Self {
            // containers_dir is ignored in embedded mode (set by ExtractedWorkspace)
            containers_dir: PathBuf::from("embedded"),
            nix_attribute: "dockerImage".to_string(),
            force_native: false,
            force_docker: true, // Always use Docker in portable mode
            nix_store_volume: None,
            timeout: Duration::from_secs(600),
            retries: RetryConfig::default(),
            max_concurrent: num_cpus::get(),
            package_registry: default_package_registry(),
        }
    }

    /// Resolve a container name (with optional version) to a root flake package name.
    ///
    /// Tries exact match with version first, then falls back to default version.
    /// Mirrors the logic in bin/build/_common.sh `resolve_package_name()`.
    ///
    /// # Examples
    /// ```
    /// use nix_container_builder::BuildConfig;
    /// let config = BuildConfig::default();
    /// assert_eq!(config.resolve_package_name("postgresql", Some("17")), Some("postgresql-17".to_string()));
    /// assert_eq!(config.resolve_package_name("postgresql", None), Some("postgresql-17".to_string()));
    /// assert_eq!(config.resolve_package_name("airflow", None), Some("airflow-3".to_string()));
    /// // An unknown VERSION falls back to the family default; it is not an error.
    /// assert_eq!(config.resolve_package_name("redis", Some("9")), Some("redis-7".to_string()));
    /// // An unknown CONTAINER is None (bash logs and returns 1).
    /// assert_eq!(config.resolve_package_name("unknown", None), None);
    /// ```
    pub fn resolve_package_name(&self, container: &str, version: Option<&str>) -> Option<String> {
        // Try exact match with version first
        if let Some(ver) = version {
            let key = format!("{}:{}", container, ver);
            if let Some(pkg) = self.package_registry.get(&key) {
                return Some(pkg.clone());
            }
        }
        // Fall back to default (no version)
        self.package_registry.get(container).cloned()
    }

    /// Set the containers directory
    pub fn with_containers_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.containers_dir = path.into();
        self
    }

    /// Set the Nix attribute to build
    pub fn with_nix_attribute(mut self, attr: impl Into<String>) -> Self {
        self.nix_attribute = attr.into();
        self
    }

    /// Force native Nix build
    pub fn with_force_native(mut self, force: bool) -> Self {
        self.force_native = force;
        self
    }

    /// Force Docker-based build
    pub fn with_force_docker(mut self, force: bool) -> Self {
        self.force_docker = force;
        self
    }

    /// Set the Nix store volume name
    pub fn with_nix_store_volume(mut self, volume: impl Into<String>) -> Self {
        self.nix_store_volume = Some(volume.into());
        self
    }

    /// Set the build timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the retry configuration
    pub fn with_retries(mut self, retries: RetryConfig) -> Self {
        self.retries = retries;
        self
    }

    /// Set maximum concurrent builds
    pub fn with_max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = max;
        self
    }
}

/// Configuration for retry behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u32,

    /// Initial delay between retries in milliseconds
    pub initial_delay_ms: u64,

    /// Maximum delay between retries in milliseconds
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 10000,
        }
    }
}

impl RetryConfig {
    /// Calculate delay for a given attempt using exponential backoff
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        let delay_ms = self.initial_delay_ms * 2u64.pow(attempt);
        Duration::from_millis(delay_ms.min(self.max_delay_ms))
    }
}

/// Serialization helpers for Duration
mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        duration.as_secs().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BuildConfig::default();
        assert_eq!(config.nix_attribute, "dockerImage");
        assert!(!config.force_native);
        assert!(!config.force_docker);
        assert_eq!(config.timeout, Duration::from_secs(600));
        assert!(!config.package_registry.is_empty());
    }

    #[test]
    fn test_builder_pattern() {
        let config = BuildConfig::new()
            .with_containers_dir("/custom/path")
            .with_nix_attribute("customAttr")
            .with_force_native(true)
            .with_timeout(Duration::from_secs(300));

        assert_eq!(config.containers_dir, PathBuf::from("/custom/path"));
        assert_eq!(config.nix_attribute, "customAttr");
        assert!(config.force_native);
        assert_eq!(config.timeout, Duration::from_secs(300));
    }

    #[test]
    fn test_resolve_package_name() {
        let config = BuildConfig::default();

        // Exact version match
        assert_eq!(config.resolve_package_name("postgresql", Some("17")), Some("postgresql-17".to_string()));
        assert_eq!(config.resolve_package_name("postgresql", Some("16")), Some("postgresql-16".to_string()));
        assert_eq!(config.resolve_package_name("redis", Some("7")), Some("redis-7".to_string()));
        assert_eq!(config.resolve_package_name("redis", Some("8")), Some("redis-8".to_string()));
        assert_eq!(config.resolve_package_name("superset", Some("4")), Some("superset-4".to_string()));

        // Default version (no version specified)
        assert_eq!(config.resolve_package_name("postgresql", None), Some("postgresql-17".to_string()));
        assert_eq!(config.resolve_package_name("redis", None), Some("redis-7".to_string()));
        assert_eq!(config.resolve_package_name("superset", None), Some("superset-5".to_string()));

        // Single-version families: the bare name resolves to the VERSIONED
        // package for every family except odoo. (This block asserted the
        // unsuffixed name for all five until Phase 10; that was drift from
        // bin/build/_common.sh, and `.#airflow`/`.#kafka` are separate flake
        // aliases, so the drift silently built a different attribute.)
        assert_eq!(config.resolve_package_name("airflow", None), Some("airflow-3".to_string()));
        assert_eq!(config.resolve_package_name("airflow", Some("3")), Some("airflow-3".to_string()));
        assert_eq!(config.resolve_package_name("kafka", None), Some("kafka-4".to_string()));
        assert_eq!(config.resolve_package_name("kafka", Some("4")), Some("kafka-4".to_string()));
        assert_eq!(config.resolve_package_name("spark", None), Some("spark-4".to_string()));
        assert_eq!(config.resolve_package_name("spark", Some("4")), Some("spark-4".to_string()));
        assert_eq!(config.resolve_package_name("jupyterhub", None), Some("jupyterhub-5".to_string()));
        assert_eq!(config.resolve_package_name("jupyterhub", Some("5")), Some("jupyterhub-5".to_string()));
        // odoo is the exception: bare `odoo` is the UNSUFFIXED package.
        assert_eq!(config.resolve_package_name("odoo", None), Some("odoo".to_string()));
        assert_eq!(config.resolve_package_name("odoo", Some("15")), Some("odoo-15".to_string()));
        assert_eq!(config.resolve_package_name("odoo", Some("18")), Some("odoo-18".to_string()));

        // Unknown container
        assert_eq!(config.resolve_package_name("unknown", None), None);
        assert_eq!(config.resolve_package_name("postgresql", Some("99")), Some("postgresql-17".to_string())); // falls back to default
    }

    #[test]
    fn test_retry_backoff() {
        let config = RetryConfig::default();

        // First retry: 1000ms
        assert_eq!(config.calculate_delay(0), Duration::from_millis(1000));

        // Second retry: 2000ms
        assert_eq!(config.calculate_delay(1), Duration::from_millis(2000));

        // Third retry: 4000ms
        assert_eq!(config.calculate_delay(2), Duration::from_millis(4000));

        // Should cap at max_delay_ms (10000)
        assert_eq!(config.calculate_delay(10), Duration::from_millis(10000));
    }
}
