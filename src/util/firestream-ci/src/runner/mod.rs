//! Pattern #4 — 4-way runner dispatch. Sentinel-gated auto-detection
//! mirroring `bin/ci/ci.sh` (the dispatcher):
//!
//! ```text
//! Order:
//!   1. Cloud Build env (BUILD_ID + BUILDER_OUTPUT) → cloudbuild
//!   2. uname -s == Darwin                          → host-darwin
//!   3. nix on PATH + devshell sentinel             → host-linux
//!   4. docker on PATH                              → docker
//!   5. otherwise                                   → error
//! ```
//!
//! Sentinels (any present ⇒ we're in the corresponding context):
//!   * `/.dockerenv` exists                         → inside a Docker container
//!   * `RUNNING_IN_DOCKER=1`                        → builder-context marker
//!   * any `profile.devshell_sentinels` entry       → inside a Nix devShell
//!   * `BUILD_ID` AND `BUILDER_OUTPUT`              → Cloud Build worker
//!   * `BUILDER_BUILD_ID`                           → Cloud Build (alt env name)
//!
//! The devshell sentinel *names* are profile data (`ci-manifest.json`'s
//! `devshell_sentinels`; see `crate::profile`), not literals compiled in here —
//! `IN_NIX_SHELL` is Nix's own and lives in the built-in default profile, while
//! a project's own marker (`FIRESTREAM_DEVSHELL=1`) comes from its profile.
//!
//! `Runner::detect()` honors the explicit `CI_RUNNER` override env var so
//! callers can pin the dispatch.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(
        "runner: no viable runner detected (set CI_RUNNER explicitly to host-linux | host-darwin | docker | cloudbuild)"
    )]
    Undetected,

    #[error("runner: unknown CI_RUNNER value `{0}`")]
    UnknownRunner(String),
}

/// The four possible execution contexts. The bash uses lowercase
/// hyphenated strings (`host-linux`, etc.); our `label()` matches them
/// byte-for-byte so `CI_RUNNER=host-linux` round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runner {
    HostLinux,
    HostDarwin,
    Docker,
    Cloudbuild,
}

impl Runner {
    pub fn label(self) -> &'static str {
        match self {
            Self::HostLinux => "host-linux",
            Self::HostDarwin => "host-darwin",
            Self::Docker => "docker",
            Self::Cloudbuild => "cloudbuild",
        }
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "host-linux" => Ok(Self::HostLinux),
            "host-darwin" => Ok(Self::HostDarwin),
            "docker" => Ok(Self::Docker),
            "cloudbuild" => Ok(Self::Cloudbuild),
            other => Err(Error::UnknownRunner(other.into())),
        }
    }

    /// Whether this runner runs inside a container.
    pub fn is_container(self) -> bool {
        matches!(self, Self::Docker | Self::Cloudbuild)
    }

    /// Detect the current runner from env + filesystem sentinels, using the
    /// built-in project-free devshell sentinel set
    /// ([`crate::profile::Profile::default`] — `IN_NIX_SHELL` only).
    ///
    /// Prefer [`Runner::detect_with_profile`] wherever a profile is in hand:
    /// a project's own devshell marker (`FIRESTREAM_DEVSHELL=1`) is profile
    /// data, and without it a host that has `nix` on PATH but is not inside
    /// `nix develop` falls through to the Docker runner.
    pub fn detect_with<F, G>(getenv: F, has_sentinel: G) -> Result<Self, Error>
    where
        F: Fn(&str) -> Option<String>,
        G: Fn(Sentinel) -> bool,
    {
        let default = crate::profile::Profile::default();
        Self::detect_with_sentinels(getenv, has_sentinel, &default.devshell_sentinels)
    }

    /// Detect using `profile.devshell_sentinels` against the real process env
    /// and filesystem.
    pub fn detect_with_profile(profile: &crate::profile::Profile) -> Result<Self, Error> {
        Self::detect_with_sentinels(
            |k| std::env::var(k).ok(),
            real_sentinel,
            &profile.devshell_sentinels,
        )
    }

    /// The full form: `getenv` and `has_sentinel` make this unit-testable
    /// without touching the real environment, and `devshell_sentinels` is the
    /// profile's list (see [`crate::profile::EnvSentinel`]).
    ///
    /// The devshell sentinel names used to be `IN_NIX_SHELL` /
    /// `CONCEPTDB_DEVSHELL` literals compiled into this function. They are now
    /// data; an empty list simply means step 4 (Linux host + devshell) can
    /// never fire, and detection falls through to the Docker runner.
    pub fn detect_with_sentinels<F, G>(
        getenv: F,
        has_sentinel: G,
        devshell_sentinels: &[crate::profile::EnvSentinel],
    ) -> Result<Self, Error>
    where
        F: Fn(&str) -> Option<String>,
        G: Fn(Sentinel) -> bool,
    {
        // Fold the closures into the shared probe set, then decide. The two
        // axes (`Runner` = where the orchestrator runs; `BuildStrategy` = how
        // one nix build executes) are deliberately expressed over ONE `Probe`
        // — see `crate::platform`.
        let probe = crate::platform::Probe {
            uname_s: if std::env::consts::OS == "macos" {
                "Darwin".to_string()
            } else {
                "Linux".to_string()
            },
            nix_on_path: has_sentinel(Sentinel::Nix),
            docker_on_path: has_sentinel(Sentinel::Docker),
            dockerenv_file: has_sentinel(Sentinel::DockerEnvFile),
            in_container: has_sentinel(Sentinel::DockerEnvFile),
            cloudbuild_env: crate::platform::detect_cloudbuild(&getenv),
            ..crate::platform::Probe::default()
        };
        Self::detect_from_probe(&probe, getenv, devshell_sentinels)
    }

    /// The runner axis over the shared [`crate::platform::Probe`].
    ///
    /// `getenv` still carries the env-only signals that are not probe state:
    /// the `CI_RUNNER` override, the `RUNNING_IN_DOCKER` builder marker, and
    /// the profile's devshell sentinel names.
    pub fn detect_from_probe<F>(
        probe: &crate::platform::Probe,
        getenv: F,
        devshell_sentinels: &[crate::profile::EnvSentinel],
    ) -> Result<Self, Error>
    where
        F: Fn(&str) -> Option<String>,
    {
        // Explicit override wins.
        if let Some(v) = getenv("CI_RUNNER") {
            if !v.trim().is_empty() {
                return Self::parse(v.trim());
            }
        }

        // 1. Cloud Build.
        if probe.cloudbuild_env {
            return Ok(Self::Cloudbuild);
        }

        // 2 + 5. If we're already inside a Docker container, that wins
        // over the host-detection path — re-entering self-as-host would
        // recursively spawn a nested container.
        //
        // Note this deliberately keys on `/.dockerenv` specifically rather
        // than `probe.in_container`: the strategy axis treats every container
        // route alike, but the runner axis must not classify e.g. a
        // systemd-nspawn host as a Docker runner it can re-enter.
        if probe.dockerenv_file || getenv("RUNNING_IN_DOCKER").as_deref() == Some("1") {
            return Ok(Self::Docker);
        }

        // 3. Darwin host. The bash gates the host runner on the OS string
        // alone, not on the Nix devshell — Darwin has no docker fallback.
        if probe.is_darwin() {
            return Ok(Self::HostDarwin);
        }

        // 4. Linux host with a devshell sentinel from the profile.
        if probe.nix_on_path && devshell_sentinels.iter().any(|s| s.is_set(&getenv)) {
            return Ok(Self::HostLinux);
        }

        // 5. Docker fallback on Linux.
        if probe.docker_on_path {
            return Ok(Self::Docker);
        }

        Err(Error::Undetected)
    }

    /// Detect using the real process env + filesystem and the built-in
    /// project-free sentinel set. Convenience for production code that has no
    /// profile in hand; tests use `detect_with` / `detect_with_sentinels`.
    pub fn detect() -> Result<Self, Error> {
        Self::detect_with(|k| std::env::var(k).ok(), real_sentinel)
    }

    /// What it would take to re-enter the target runner from this one.
    /// `None` ⇒ no re-entry needed (already in target context, or the
    /// dispatch is a no-op).
    pub fn requires_reenter(self, target: Runner) -> Option<ReenterPlan> {
        if self == target {
            return None;
        }
        let from = self;
        let kind = match (from, target) {
            (Runner::HostLinux, Runner::Docker)
            | (Runner::HostDarwin, Runner::Docker)
            | (Runner::HostLinux, Runner::Cloudbuild)
            | (Runner::HostDarwin, Runner::Cloudbuild) => ReenterKind::SpawnContainer,
            (Runner::Docker, Runner::HostLinux) | (Runner::Cloudbuild, Runner::HostLinux) => {
                ReenterKind::DescendToHost
            }
            // Unusual transitions (host-linux → host-darwin, etc.) are
            // logically impossible on a single host. Surface as "manual"
            // so callers don't accidentally try to auto-handle them.
            _ => ReenterKind::Manual,
        };
        Some(ReenterPlan { from, target, kind })
    }
}

/// Sentinel categories the detector probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sentinel {
    /// `/.dockerenv` file (created by Docker daemon inside every container).
    DockerEnvFile,
    /// `nix` binary on PATH.
    Nix,
    /// `docker` binary on PATH.
    Docker,
}

/// What a `Runner::requires_reenter` returned. The reenter module
/// (Pattern #3) consumes this to compose the actual re-exec.
#[derive(Debug, Clone)]
pub struct ReenterPlan {
    pub from: Runner,
    pub target: Runner,
    pub kind: ReenterKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReenterKind {
    /// Spawn a container that re-runs the caller's argv inside it.
    SpawnContainer,
    /// Descend to host execution (the container case is a no-op; the
    /// host case just continues in-place).
    DescendToHost,
    /// Caller must handle the transition itself (cross-OS host hops,
    /// runner combinations we don't model).
    Manual,
}

/// The production `has_sentinel` probe: real filesystem + real PATH.
fn real_sentinel(s: Sentinel) -> bool {
    match s {
        Sentinel::DockerEnvFile => std::path::Path::new("/.dockerenv").exists(),
        Sentinel::Nix => which("nix"),
        Sentinel::Docker => which("docker"),
    }
}

fn which(program: &str) -> bool {
    // One implementation, in the platform authority.
    crate::platform::on_path(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| m.get(k).cloned()
    }

    fn no_sentinels(_s: Sentinel) -> bool {
        false
    }

    #[test]
    fn explicit_ci_runner_overrides_detection() {
        let env = env_map(&[("CI_RUNNER", "host-darwin")]);
        let r = Runner::detect_with(env, no_sentinels).unwrap();
        assert_eq!(r, Runner::HostDarwin);
    }

    #[test]
    fn detects_cloudbuild_by_build_id_and_builder_output() {
        let env = env_map(&[("BUILD_ID", "x"), ("BUILDER_OUTPUT", "/out")]);
        let r = Runner::detect_with(env, no_sentinels).unwrap();
        assert_eq!(r, Runner::Cloudbuild);
    }

    #[test]
    fn detects_cloudbuild_by_builder_build_id() {
        let env = env_map(&[("BUILDER_BUILD_ID", "x")]);
        let r = Runner::detect_with(env, no_sentinels).unwrap();
        assert_eq!(r, Runner::Cloudbuild);
    }

    #[test]
    fn detects_docker_via_running_in_docker_env() {
        let env = env_map(&[("RUNNING_IN_DOCKER", "1")]);
        let r = Runner::detect_with(env, no_sentinels).unwrap();
        assert_eq!(r, Runner::Docker);
    }

    #[test]
    fn detects_docker_via_dockerenv_sentinel() {
        let env = env_map(&[]);
        let s = |s: Sentinel| matches!(s, Sentinel::DockerEnvFile);
        let r = Runner::detect_with(env, s).unwrap();
        assert_eq!(r, Runner::Docker);
    }

    #[test]
    fn label_round_trips_via_parse() {
        for r in [
            Runner::HostLinux,
            Runner::HostDarwin,
            Runner::Docker,
            Runner::Cloudbuild,
        ] {
            assert_eq!(Runner::parse(r.label()).unwrap(), r);
        }
    }

    #[test]
    fn unknown_runner_errors() {
        assert!(Runner::parse("frobnitz").is_err());
        assert!(matches!(
            Runner::parse("frobnitz"),
            Err(Error::UnknownRunner(_))
        ));
    }

    #[test]
    fn requires_reenter_same_runner_is_none() {
        assert!(
            Runner::HostLinux
                .requires_reenter(Runner::HostLinux)
                .is_none()
        );
        assert!(Runner::Docker.requires_reenter(Runner::Docker).is_none());
    }

    #[test]
    fn requires_reenter_host_to_container_spawns() {
        let plan = Runner::HostLinux.requires_reenter(Runner::Docker).unwrap();
        assert_eq!(plan.from, Runner::HostLinux);
        assert_eq!(plan.target, Runner::Docker);
        assert_eq!(plan.kind, ReenterKind::SpawnContainer);
    }

    #[test]
    fn requires_reenter_container_to_host_descends() {
        let plan = Runner::Docker.requires_reenter(Runner::HostLinux).unwrap();
        assert_eq!(plan.kind, ReenterKind::DescendToHost);
    }

    #[test]
    fn requires_reenter_cross_os_is_manual() {
        let plan = Runner::HostLinux
            .requires_reenter(Runner::HostDarwin)
            .unwrap();
        assert_eq!(plan.kind, ReenterKind::Manual);
    }

    #[test]
    fn is_container_classification() {
        assert!(Runner::Docker.is_container());
        assert!(Runner::Cloudbuild.is_container());
        assert!(!Runner::HostLinux.is_container());
        assert!(!Runner::HostDarwin.is_container());
    }

    #[test]
    fn undetected_on_bare_unknown_host() {
        // No env, no sentinels, no OS auto-detect path (we can't override
        // std::env::consts::OS from a unit test, so this assertion holds
        // only when running on Linux without nix/docker AND when not
        // shadowed by macOS). Use a guard: if `consts::OS == "macos"`,
        // the detection will return HostDarwin without error.
        if std::env::consts::OS == "macos" {
            let env = env_map(&[]);
            let r = Runner::detect_with(env, no_sentinels).unwrap();
            assert_eq!(r, Runner::HostDarwin);
        } else {
            let env = env_map(&[]);
            let r = Runner::detect_with(env, no_sentinels);
            assert!(matches!(r, Err(Error::Undetected)));
        }
    }
}
