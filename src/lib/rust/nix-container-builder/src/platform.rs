//! Platform and architecture detection
//!
//! # Status: scheduled for deletion, deliberately not deleted yet
//!
//! This module is the **fourth** copy of the "native or Docker?" predicate. The
//! authority is the golden-vector file `bin/build/strategy-cases.json`, driven
//! by two harnesses: `bin/build/test-strategy-parity.sh` (over
//! `bin/build/strategy.sh`) and `firestream_ci::platform::Probe` (over
//! `src/util/firestream-ci/src/platform/parity.rs`), 45 cases.
//!
//! The end state is for this module to **delegate** to `firestream_ci::platform`
//! and shrink to a thin adapter. That merge is *not* done here, on purpose:
//!
//! * `firestream-ci` lives in `src/util`, a deliberately isolated Cargo
//!   workspace (edition 2024, its own lockfile, its own 21-member-free root).
//!   `nix-container-builder` is a **root workspace** member. A cargo dependency
//!   edge would drag ~15 duplicate transitive majors (tonic, axum x2, reqwest,
//!   rustls, bollard, git2, superconsole, ratatui) into the root lock. The
//!   isolation is load-bearing, not incidental.
//! * The precondition written into the plan — "only after Part D proves the
//!   merged predicate in production" — is **not met**. `bin/build/container-images.sh`
//!   is still the default build path; the Rust path is opt-in behind
//!   `FIRESTREAM_BUILD_IMPL=rust` (a strangler), and no full Firestream container
//!   build has yet run through it on either Linux or Darwin.
//!
//! Until both hold, this copy is kept **checked instead of trusted**:
//! `tests/platform_golden_vectors.rs` reads the same `strategy-cases.json` via
//! `include_str!` and asserts agreement on the subset this module can express —
//! zero dependency edges, and it already caught one real defect (a missing
//! `containerd` cgroup route, see [`is_running_in_container`]).
//!
//! # Delegation map (mechanical, when the preconditions are met)
//!
//! | here | `firestream_ci::platform` |
//! |---|---|
//! | [`PlatformInfo::can_build_native`] | `Probe::can_build_native(target_arch)` |
//! | [`is_running_in_container`] | `detect_in_container(probe_root)` |
//! | [`PlatformInfo::default_nix_store_volume`] | `platform::nix_store_volume(arch)` |
//! | [`Architecture::docker_platform`] | `platform::docker_platform(arch)` |
//! | [`check_nix_available`] / [`check_docker_available`] | `Probe::detect_with_docker_daemon` |
//! | [`PlatformInfo::recommended_strategy`] | `Probe::decide(...)` — **see the ladder decision on that method** |
//!
//! # Known deltas, all narrowing (this module is the weaker one)
//!
//! 1. **No escape-hatch env var.** The authority reads `FIRESTREAM_BUILD_STRATEGY`
//!    (rung 1) and the CI profile's `build_strategy.default` (rung 4) *above*
//!    probing. This module has neither; `--native`/`--docker` on the CLI are the
//!    only overrides. Delegation gains both for free.
//! 2. **Arch-blind.** [`PlatformInfo::can_build_native`] takes no target arch, so
//!    it cannot express blocker #2 (`cross-arch target (T) differs from host
//!    arch (H)`). Cross-arch builds reach the Docker strategy today only because
//!    the caller happens to force it.
//! 3. **No `/nix/store` check.** [`check_nix_available`] is `which nix` only; the
//!    authority additionally requires `/nix/store` to be a directory (blocker
//!    #4). A host with the Nix *client* and no store answers `native` here and
//!    `docker` there. This is the one delta that could bite in practice; fixing
//!    it properly means adding a field to [`PlatformInfo`], which is a public
//!    struct with literal constructions in this crate's tests — left for the
//!    delegation commit rather than done twice.
//! 4. **No blocker strings.** The authority returns a human-readable reason
//!    native was rejected; this module returns a bare bool, so `firestream build`
//!    cannot tell the user *why* it fell back.

use crate::error::{NixContainerError, Result};
use crate::strategy::BuildStrategy;
use std::path::Path;
use tokio::process::Command;

/// Operating system platform
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Darwin,
}

impl Platform {
    /// Detect the current platform
    pub fn detect() -> Result<Self> {
        match std::env::consts::OS {
            "linux" => Ok(Platform::Linux),
            "macos" => Ok(Platform::Darwin),
            other => Err(NixContainerError::UnsupportedPlatform(other.to_string())),
        }
    }

    /// Check if this is Linux
    pub fn is_linux(&self) -> bool {
        matches!(self, Platform::Linux)
    }

    /// Check if this is macOS/Darwin
    pub fn is_darwin(&self) -> bool {
        matches!(self, Platform::Darwin)
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Linux => write!(f, "Linux"),
            Platform::Darwin => write!(f, "macOS"),
        }
    }
}

/// CPU architecture
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    X86_64,
    Aarch64,
}

impl Architecture {
    /// Detect the current architecture
    pub fn detect() -> Result<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Ok(Architecture::X86_64),
            "aarch64" => Ok(Architecture::Aarch64),
            other => Err(NixContainerError::UnsupportedPlatform(format!(
                "Unsupported architecture: {}",
                other
            ))),
        }
    }

    /// Get the Docker platform string for this architecture
    pub fn docker_platform(&self) -> &'static str {
        match self {
            Architecture::X86_64 => "linux/amd64",
            Architecture::Aarch64 => "linux/arm64",
        }
    }

    /// Get the Nix store volume suffix for this architecture
    pub fn volume_suffix(&self) -> &'static str {
        match self {
            Architecture::X86_64 => "amd64",
            Architecture::Aarch64 => "arm64",
        }
    }
}

impl std::fmt::Display for Architecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Architecture::X86_64 => write!(f, "x86_64"),
            Architecture::Aarch64 => write!(f, "aarch64"),
        }
    }
}

/// Complete platform information
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    /// Operating system platform
    pub platform: Platform,

    /// CPU architecture
    pub arch: Architecture,

    /// Whether Nix is available in PATH
    pub nix_available: bool,

    /// Whether Docker is available and accessible
    pub docker_available: bool,

    /// Whether we're running inside a container
    pub in_container: bool,
}

impl PlatformInfo {
    /// Detect all platform information
    pub async fn detect() -> Result<Self> {
        let platform = Platform::detect()?;
        let arch = Architecture::detect()?;

        let nix_available = check_nix_available().await;
        let docker_available = check_docker_available().await;
        let in_container = is_running_in_container();

        Ok(Self {
            platform,
            arch,
            nix_available,
            docker_available,
            in_container,
        })
    }

    /// Get the recommended build strategy for this platform
    ///
    /// # DECISION (Phase 10): the docker-availability fallback ladder is DROPPED at delegation
    ///
    /// The ladder below has four rungs:
    ///
    /// ```text
    ///   1. Linux + nix + !container  -> Native
    ///   2. else docker_available     -> Docker
    ///   3. else nix_available        -> Native      <-- the contested rung
    ///   4. else                      -> Docker
    /// ```
    ///
    /// Neither `bin/build/strategy.sh` nor `firestream_ci::platform` has rung 3
    /// (or, equivalently, rung 2's guard): both simply answer `docker` when
    /// native is blocked and let the Docker build primitive fail on a missing
    /// daemon. `firestream_ci::platform::Probe` carries a `docker_daemon_up`
    /// field that *could* receive it. **It should not.** Rung 3 is a difference
    /// to drop, not a feature to lift:
    ///
    /// * **It cannot be expressed correctly.** This method takes no target
    ///   architecture, so its rung 1 is a strictly weaker predicate than
    ///   `native_blocker()`. Lifting rung 3 into the shared predicate means
    ///   making it arch-aware, and at that point rung 3 reads: "native is
    ///   *impossible* for this target — cross-arch, or macOS where the image
    ///   derivations are gated behind `isLinux` — so let's do it natively
    ///   anyway." That is wrong by construction, not merely unhelpful.
    /// * **It only changes which error you get.** Rung 3 fires exactly when
    ///   native is blocked AND there is no reachable Docker daemon. Both
    ///   outcomes fail. Rung 3 picks the *worse* failure: on macOS,
    ///   `unavailable "<pkg>"` from the flake stub, versus Docker's "Cannot
    ///   connect to the Docker daemon", which names the actually-missing
    ///   dependency and is directly actionable.
    /// * **It is expensive to gate.** Adding it would require new golden
    ///   vectors on *both* sides of `bin/build/test-strategy-parity.sh`, plus a
    ///   `docker info` round trip in the shell predicate that
    ///   `bin/build/strategy.sh` deliberately does not make (it is a
    ///   multi-second stall on a host with no daemon, which is precisely the
    ///   host this rung claims to help). Paying that to choose a failure message
    ///   is a bad trade.
    ///
    /// A caller that genuinely wants to pre-empt the "no daemon" case should ask
    /// [`PlatformInfo::can_build_docker`] (`Probe::can_build_docker` /
    /// `docker_daemon_up` after the shared side) and emit a *good* error, rather
    /// than silently rerouting to a strategy the probe already rejected.
    ///
    /// The ladder is left intact here so this commit changes no behaviour; it is
    /// removed, not ported, when this module becomes an adapter. See the module
    /// docs for the rest of the delegation map.
    pub fn recommended_strategy(&self) -> BuildStrategy {
        // On Linux with native Nix, prefer native builds
        if self.platform.is_linux() && self.nix_available && !self.in_container {
            BuildStrategy::NativeNix
        }
        // On macOS or in containers, use Docker-based builds
        else if self.docker_available {
            BuildStrategy::DockerNix
        }
        // Fallback to native if available
        else if self.nix_available {
            BuildStrategy::NativeNix
        }
        // Default to Docker
        else {
            BuildStrategy::DockerNix
        }
    }

    /// Get the Docker platform string
    pub fn docker_platform(&self) -> &'static str {
        self.arch.docker_platform()
    }

    /// Get the default Nix store volume name
    pub fn default_nix_store_volume(&self) -> String {
        format!("firestream-nix-store-{}", self.arch.volume_suffix())
    }

    /// Check if native Nix builds are possible
    pub fn can_build_native(&self) -> bool {
        self.platform.is_linux() && self.nix_available && !self.in_container
    }

    /// Check if Docker-based builds are possible
    pub fn can_build_docker(&self) -> bool {
        self.docker_available
    }
}

/// Check if Nix is available in PATH
async fn check_nix_available() -> bool {
    which::which("nix").is_ok()
}

/// Check if Docker is available and the daemon is accessible
async fn check_docker_available() -> bool {
    if which::which("docker").is_err() {
        return false;
    }

    // Try to ping Docker daemon
    let output = Command::new("docker")
        .args(["info"])
        .output()
        .await;

    matches!(output, Ok(o) if o.status.success())
}

/// Check if we're running inside a container
fn is_running_in_container() -> bool {
    // Check for .dockerenv file
    if Path::new("/.dockerenv").exists() {
        return true;
    }

    // Check for Kubernetes pod
    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
        return true;
    }

    // Check common container environment variables
    if std::env::var("CONTAINER").is_ok()
        || std::env::var("container").is_ok()
        || std::env::var("CONTAINER_REGISTRY_URL").is_ok()
    {
        return true;
    }

    // Check cgroups for container indicators.
    //
    // `containerd` was missing here until Phase 10 and is not optional in this
    // project: k3d/k3s pods run under the containerd runtime, so a devcontainer
    // or CI pod whose /proc/1/cgroup says `containerd` (and which has neither
    // /.dockerenv nor KUBERNETES_SERVICE_HOST) was reported as "on the host" and
    // would have attempted a native build against a store it does not own.
    // `bin/build/strategy.sh`'s `fs_in_container` and
    // `firestream_ci::platform::detect_in_container` both match all three;
    // gated by tests/platform_golden_vectors.rs.
    if let Ok(cgroups) = std::fs::read_to_string("/proc/1/cgroup") {
        if cgroups.contains("docker")
            || cgroups.contains("kubepods")
            || cgroups.contains("containerd")
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_detection() {
        let platform = Platform::detect();
        assert!(platform.is_ok());
    }

    #[test]
    fn test_architecture_detection() {
        let arch = Architecture::detect();
        assert!(arch.is_ok());
    }

    #[test]
    fn test_docker_platform_string() {
        assert_eq!(Architecture::X86_64.docker_platform(), "linux/amd64");
        assert_eq!(Architecture::Aarch64.docker_platform(), "linux/arm64");
    }

    #[test]
    fn test_volume_suffix() {
        assert_eq!(Architecture::X86_64.volume_suffix(), "amd64");
        assert_eq!(Architecture::Aarch64.volume_suffix(), "arm64");
    }
}
