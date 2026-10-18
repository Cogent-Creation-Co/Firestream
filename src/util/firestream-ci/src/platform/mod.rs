//! The single authority for "where am I, and how should this Nix build run?"
//!
//! ## Two axes over one probe set
//!
//! * [`Runner`](crate::runner::Runner) — *where the CI orchestrator itself
//!   runs*: host-linux / host-darwin / docker / cloudbuild.
//! * [`BuildStrategy`] — *how one `nix build` executes*: on the host
//!   `/nix/store`, or inside a `nixos/nix` container with a persistent volume.
//!
//! Both are decided from one [`Probe`]. `Probe` is a plain data struct: it is
//! constructible from fixture values (the parity vectors never touch the real
//! machine) and, separately, from the live host via [`Probe::detect`].
//!
//! ## Merged from three prior copies
//!
//! | Source | What it contributed |
//! |---|---|
//! | `nix-container-builder/src/platform.rs` | `can_build_native`, `recommended_strategy`, `default_nix_store_volume`, `is_running_in_container`, the nix/docker availability checks. Richest of the three; the base. |
//! | `crate::runner::Runner::detect` | the 4-way orchestration context, `CI_RUNNER` override, profile-sourced devshell sentinels. |
//! | `bin/build/strategy.sh` | the zero-dependency shell mirror; its blocker strings and probe order are the wire contract. |
//!
//! `nix-container-builder` is **not** yet delegating here — that is sequenced
//! last (plan Phase 10) so the root workspace does not take a dependency on
//! `src/util` before the merged predicate is proven.
//!
//! Phase 10 ran and **did not wire the dependency**: the precondition ("only
//! after Part D proves the merged predicate in production") is unmet — the Rust
//! build path is still opt-in behind `FIRESTREAM_BUILD_IMPL=rust` and no full
//! container build has gone through it on either OS. Instead the delegation map
//! and the full list of semantic deltas were written down at the *other* end,
//! in `nix-container-builder/src/platform.rs`'s module docs, and that crate now
//! gates its own copy against `bin/build/strategy-cases.json` with a
//! dependency-free `include_str!` harness
//! (`nix-container-builder/tests/platform_golden_vectors.rs`).
//!
//! One decision recorded there concerns this module directly:
//! `PlatformInfo::recommended_strategy`'s **docker-availability fallback
//! ladder** (`else nix_available -> Native` when native is blocked and no
//! daemon is reachable) is **NOT** being lifted into this predicate.
//! [`Probe::docker_daemon_up`] stays a field callers may consult to emit a good
//! error; it must not become a rung that reroutes to a strategy
//! [`Probe::native_blocker`] has already rejected. Full reasoning lives on
//! `recommended_strategy`. Consequence for this file: **no new golden vectors,
//! `bin/build/strategy-cases.json` is unchanged, and the 45-case gate stands as
//! is.**
//!
//! ## Precedence (exact, and load-bearing)
//!
//! ```text
//!   1. FIRESTREAM_BUILD_STRATEGY = native | docker   -> forced, NO probing
//!   2. FIRESTREAM_BUILD_STRATEGY = <unrecognised>    -> warn, fall through as auto
//!   3. FIRESTREAM_BUILD_STRATEGY = auto | "" | unset -> fall through
//!   4. profile.build_strategy.default = native|docker -> forced, NO probing
//!   5. profile.build_strategy.default = auto/""/other -> (warn if other) fall through
//!   6. probe: native iff native_blocker() is None
//! ```
//!
//! Step 1 is the total-rollback escape hatch from the trinket plan and must
//! never be reordered below anything. Step 4 is this phase's addition: the CI
//! profile (`ci-manifest.json` `build_strategy.default`, parsed since Phase 4
//! and previously unread) gets a say, but strictly *under* the env var so a
//! developer can always override a profile, and strictly *over* probing so a
//! profile can pin a fleet.
//!
//! `bin/build/strategy.sh` has no profile. That is exactly
//! `build_strategy.default = "auto"`, i.e. steps 4–5 collapse to a no-op, and
//! the two implementations agree on every other input. See
//! `bin/build/strategy-cases.json` (the golden vectors) and
//! `bin/build/test-strategy-parity.sh` (the shell harness).
//!
//! ## Blocker strings
//!
//! [`Probe::native_blocker`] returns the shell's string **verbatim**, in the
//! shell's probe order. These are a contract, not diagnostics:
//!
//! 1. `host is not Linux (uname -s = {S}); image derivations are gated behind isLinux`
//! 2. `cross-arch target ({T}) differs from host arch ({H})`
//! 3. `nix not found on PATH`
//! 4. `/nix/store does not exist on this host`
//! 5. `running inside a container`

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod parity;

/// The escape-hatch env var. `auto` | `native` | `docker`.
pub const ENV_BUILD_STRATEGY: &str = "FIRESTREAM_BUILD_STRATEGY";

/// Test seam mirroring `bin/build/strategy.sh`'s `fs_probe_root`: prefixes the
/// three absolute filesystem probes (`/.dockerenv`, `/proc/1/cgroup`,
/// `/nix/store`). Empty in production.
pub const ENV_PROBE_ROOT: &str = "FIRESTREAM_PROBE_ROOT";

// ───────────────────────────────────────────────────────────────────────────
// Arch helpers — mirrors of fs_norm_arch / get_nix_volume / fs_docker_platform
// ───────────────────────────────────────────────────────────────────────────

/// `amd64 | x64 | x86_64` → `x86_64`; `arm64 | aarch64` → `aarch64`; else
/// pass-through. Mirror of `fs_norm_arch`.
pub fn norm_arch(arch: &str) -> String {
    match arch {
        "amd64" | "x64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => other,
    }
    .to_string()
}

/// The **Docker-flavoured** arch alias: `x86_64|amd64` → `amd64`,
/// `aarch64|arm64` → `arm64`, anything else passes through.
///
/// This is emphatically NOT [`norm_arch`], which goes the other way
/// (`amd64` → `x86_64`). Both mappings are live and they are inverses over the
/// two arches that matter, which is exactly why the distinction has to be
/// named: expanding a `{arch}` template with the wrong one produces
/// `firestream-nix-store-x86_64`, a *plausible-looking but different* Docker
/// volume from the `firestream-nix-store-amd64` the shell path has been
/// filling for months, and the symptom is a silently cold Nix cache.
///
/// Note it matches on the **raw** arch, not the normalised one: `x64` is not
/// folded to `amd64`. Preserved bug-for-bug from `get_nix_volume`.
pub fn docker_arch_alias(arch: &str) -> String {
    match arch {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        other => other,
    }
    .to_string()
}

/// Per-arch persistent Docker volume name. Mirror of `get_nix_volume`.
pub fn nix_store_volume(arch: &str) -> String {
    format!("firestream-nix-store-{}", docker_arch_alias(arch))
}

/// Docker `--platform` string. Mirror of `fs_docker_platform`.
pub fn docker_platform(arch: &str) -> String {
    format!("linux/{}", docker_arch_alias(arch))
}

/// Expand a CI profile's `build_strategy.docker_cache_volume` template.
///
/// `{arch}` expands via [`docker_arch_alias`], so the default template
/// `firestream-nix-store-{arch}` resolves to precisely what
/// `get_nix_volume` in `bin/build/strategy.sh` produces. `{system}` and
/// `{project}` are passed through untouched here — a caller that needs them
/// should run [`crate::profile::Profile::expand`] first, or simply not use
/// them in this field.
///
/// An empty template yields an empty string, meaning "no cache volume": the
/// caller must then mount nothing rather than mount `":/nix"`.
pub fn docker_cache_volume(template: &str, arch: &str) -> String {
    if template.is_empty() {
        return String::new();
    }
    template.replace("{arch}", &docker_arch_alias(arch))
}

// ───────────────────────────────────────────────────────────────────────────
// BuildStrategy
// ───────────────────────────────────────────────────────────────────────────

/// How a single `nix build` executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildStrategy {
    /// Host `/nix/store`, `nix build --out-link`.
    Native,
    /// `nixos/nix` container with a persistent per-arch `/nix` volume.
    Docker,
}

impl BuildStrategy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Docker => "docker",
        }
    }
}

impl std::fmt::Display for BuildStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Parse result for `FIRESTREAM_BUILD_STRATEGY` / `build_strategy.default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyOverride {
    /// Probe. (`auto`, empty, unset — and, after a warning, anything unknown.)
    Auto,
    /// Short-circuit; do not probe.
    Force(BuildStrategy),
}

impl StrategyOverride {
    /// `None` / `Some("")` / `Some("auto")` → `Auto` with no warning.
    /// `Some("native"|"docker")` → `Force`. Anything else → `Auto` plus the
    /// warning text (byte-identical to `fs_choose_strategy`'s `log_warn`).
    pub fn parse_env(raw: Option<&str>) -> (Self, Option<String>) {
        match raw {
            None | Some("") | Some("auto") => (Self::Auto, None),
            Some("native") => (Self::Force(BuildStrategy::Native), None),
            Some("docker") => (Self::Force(BuildStrategy::Docker), None),
            Some(other) => (
                Self::Auto,
                Some(format!(
                    "Unknown {ENV_BUILD_STRATEGY}='{other}' (expected auto|native|docker); treating as auto"
                )),
            ),
        }
    }

    /// Same grammar, different warning text — this one names the profile, not
    /// the env var, so a malformed `ci-manifest.json` is diagnosable.
    pub fn parse_profile(raw: Option<&str>) -> (Self, Option<String>) {
        match raw {
            None | Some("") | Some("auto") => (Self::Auto, None),
            Some("native") => (Self::Force(BuildStrategy::Native), None),
            Some("docker") => (Self::Force(BuildStrategy::Docker), None),
            Some(other) => (
                Self::Auto,
                Some(format!(
                    "Unknown build_strategy.default='{other}' in the CI profile (expected auto|native|docker); treating as auto"
                )),
            ),
        }
    }
}

/// Which rung of the precedence ladder produced a [`Decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    /// `FIRESTREAM_BUILD_STRATEGY` (rung 1).
    EnvOverride,
    /// The CI profile's `build_strategy.default` (rung 4).
    ProfileDefault,
    /// The probe (rung 6).
    Probed,
}

/// The outcome of the predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub strategy: BuildStrategy,
    /// Why native is impossible for this probe+target — **independent of any
    /// override**, so a forced `native` still reports the truth. `None` means
    /// native is genuinely available.
    pub blocker: Option<String>,
    pub source: DecisionSource,
    /// Warnings emitted while parsing overrides (unknown values).
    pub warnings: Vec<String>,
}

// ───────────────────────────────────────────────────────────────────────────
// Probe
// ───────────────────────────────────────────────────────────────────────────

/// The shared probe set. Every field is raw observed state; all interpretation
/// lives in the methods, so a fixture and the live host go through identical
/// decision code.
///
/// The plan names the fields `is_linux` / `host_arch`; they are methods here
/// instead ([`Probe::is_linux`], [`Probe::host_arch`]) because blocker string
/// #1 interpolates the **raw** `uname -s` value and #2 interpolates the
/// **normalised** arch — collapsing either to a bool/normalised field would
/// lose the bytes the contract needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// `uname -s`, verbatim.
    pub uname_s: String,
    /// `uname -m`, verbatim (pre-normalisation).
    pub uname_m: String,
    /// `nix` resolves on `PATH`.
    pub nix_on_path: bool,
    /// `/nix/store` is a directory.
    pub nix_store_present: bool,
    /// `docker` resolves on `PATH`.
    pub docker_on_path: bool,
    /// `docker info` succeeds. Expensive; see [`Probe::detect`].
    pub docker_daemon_up: bool,
    /// Any of the five container routes fired.
    pub in_container: bool,
    /// `/.dockerenv` specifically — the runner axis distinguishes "a Docker
    /// container" from "any container", where the strategy axis does not.
    pub dockerenv_file: bool,
    /// Cloud Build worker env (`BUILD_ID`+`BUILDER_OUTPUT`, or `BUILDER_BUILD_ID`).
    pub cloudbuild_env: bool,
}

impl Default for Probe {
    /// A clean Linux x86_64 host that can build natively.
    fn default() -> Self {
        Self {
            uname_s: "Linux".into(),
            uname_m: "x86_64".into(),
            nix_on_path: true,
            nix_store_present: true,
            docker_on_path: true,
            docker_daemon_up: true,
            in_container: false,
            dockerenv_file: false,
            cloudbuild_env: false,
        }
    }
}

impl Probe {
    pub fn is_linux(&self) -> bool {
        self.uname_s == "Linux"
    }

    pub fn is_darwin(&self) -> bool {
        self.uname_s == "Darwin"
    }

    /// Normalised host architecture. Mirror of `fs_host_arch`.
    pub fn host_arch(&self) -> String {
        norm_arch(&self.uname_m)
    }

    /// Mirror of `fs_native_blocker`. `target_arch` of `None` or `Some("")`
    /// means "the host arch", matching the shell's `${1:-$(uname -m)}`.
    ///
    /// Probe order is the contract; see the module docs.
    pub fn native_blocker(&self, target_arch: Option<&str>) -> Option<String> {
        // 1. not Linux
        if !self.is_linux() {
            return Some(format!(
                "host is not Linux (uname -s = {}); image derivations are gated behind isLinux",
                self.uname_s
            ));
        }
        // 2. cross-arch
        let raw_target = match target_arch {
            Some(t) if !t.is_empty() => t,
            _ => self.uname_m.as_str(),
        };
        let target = norm_arch(raw_target);
        let host = self.host_arch();
        if !target.is_empty() && target != host {
            return Some(format!(
                "cross-arch target ({target}) differs from host arch ({host})"
            ));
        }
        // 3. no nix
        if !self.nix_on_path {
            return Some("nix not found on PATH".to_string());
        }
        // 4. no /nix/store
        if !self.nix_store_present {
            return Some("/nix/store does not exist on this host".to_string());
        }
        // 5. containerised
        if self.in_container {
            return Some("running inside a container".to_string());
        }
        None
    }

    /// Mirror of `fs_native_blocker` being empty.
    pub fn can_build_native(&self, target_arch: Option<&str>) -> bool {
        self.native_blocker(target_arch).is_none()
    }

    /// Docker builds need a reachable daemon.
    pub fn can_build_docker(&self) -> bool {
        self.docker_daemon_up
    }

    /// Persistent Docker volume for a target arch (defaults to the host's).
    pub fn nix_store_volume(&self, target_arch: Option<&str>) -> String {
        nix_store_volume(match target_arch {
            Some(t) if !t.is_empty() => t,
            _ => self.uname_m.as_str(),
        })
    }

    /// Docker `--platform` for a target arch (defaults to the host's).
    pub fn docker_platform(&self, target_arch: Option<&str>) -> String {
        docker_platform(match target_arch {
            Some(t) if !t.is_empty() => t,
            _ => self.uname_m.as_str(),
        })
    }

    // ── live construction ──────────────────────────────────────────────
    //
    // Kept strictly separate from the decision logic above so that the parity
    // vectors exercise the same code paths without touching the machine.

    /// Probe the live host. Does **not** run `docker info` — that is a
    /// multi-second round trip on a host with no daemon, and nothing on the
    /// strategy axis needs it. Use [`Probe::detect_with_docker_daemon`] when
    /// the answer matters.
    pub fn detect() -> Self {
        let root = std::env::var(ENV_PROBE_ROOT).unwrap_or_default();
        Self {
            uname_s: uname_s(),
            uname_m: uname_m(),
            nix_on_path: on_path("nix"),
            nix_store_present: Path::new(&format!("{root}/nix/store")).is_dir(),
            docker_on_path: on_path("docker"),
            docker_daemon_up: false,
            in_container: detect_in_container(&root),
            dockerenv_file: Path::new(&format!("{root}/.dockerenv")).exists(),
            cloudbuild_env: detect_cloudbuild(|k| std::env::var(k).ok()),
        }
    }

    /// [`Probe::detect`] plus a `docker info` round trip.
    pub fn detect_with_docker_daemon() -> Self {
        let mut p = Self::detect();
        p.docker_daemon_up = p.docker_on_path && docker_daemon_reachable();
        p
    }
}

/// Mirror of `fs_in_container`, route order included.
fn detect_in_container(root: &str) -> bool {
    if Path::new(&format!("{root}/.dockerenv")).exists() {
        return true;
    }
    for var in ["KUBERNETES_SERVICE_HOST", "CONTAINER", "container"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return true;
            }
        }
    }
    let cgroup = format!("{root}/proc/1/cgroup");
    if let Ok(text) = std::fs::read_to_string(&cgroup) {
        if text.contains("docker") || text.contains("kubepods") || text.contains("containerd") {
            return true;
        }
    }
    false
}

pub(crate) fn detect_cloudbuild<F>(getenv: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    (getenv("BUILD_ID").is_some() && getenv("BUILDER_OUTPUT").is_some())
        || getenv("BUILDER_BUILD_ID").is_some()
}

fn uname_s() -> String {
    match std::env::consts::OS {
        "linux" => "Linux".to_string(),
        "macos" => "Darwin".to_string(),
        other => {
            let mut c = other.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    }
}

fn uname_m() -> String {
    std::env::consts::ARCH.to_string()
}

pub(crate) fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        let candidate: PathBuf = dir.join(program);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

fn docker_daemon_reachable() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ───────────────────────────────────────────────────────────────────────────
// The predicate
// ───────────────────────────────────────────────────────────────────────────

/// The pure decision. `env_strategy` is the raw `FIRESTREAM_BUILD_STRATEGY`
/// value (`None` = unset); `profile_default` is the raw
/// `build_strategy.default` (`None` = no profile in hand, which is identical to
/// `"auto"` and is what `bin/build/strategy.sh` always is).
///
/// `blocker` is filled in unconditionally because `probe` is already in hand —
/// unlike the shell, evaluating it costs nothing and is not a "probe". See
/// [`decide_live`] for the ordering guarantee against the real host.
pub fn choose_strategy(
    probe: &Probe,
    target_arch: Option<&str>,
    env_strategy: Option<&str>,
    profile_default: Option<&str>,
) -> Decision {
    let blocker = probe.native_blocker(target_arch);
    let mut warnings = Vec::new();

    // Rungs 1–3: the escape hatch, read before anything else.
    let (env_ovr, env_warn) = StrategyOverride::parse_env(env_strategy);
    if let Some(w) = env_warn {
        warnings.push(w);
    }
    if let StrategyOverride::Force(s) = env_ovr {
        return Decision {
            strategy: s,
            blocker,
            source: DecisionSource::EnvOverride,
            warnings,
        };
    }

    // Rungs 4–5: the CI profile.
    let (prof_ovr, prof_warn) = StrategyOverride::parse_profile(profile_default);
    if let Some(w) = prof_warn {
        warnings.push(w);
    }
    if let StrategyOverride::Force(s) = prof_ovr {
        return Decision {
            strategy: s,
            blocker,
            source: DecisionSource::ProfileDefault,
            warnings,
        };
    }

    // Rung 6: probe.
    let strategy = if blocker.is_none() {
        BuildStrategy::Native
    } else {
        BuildStrategy::Docker
    };
    Decision {
        strategy,
        blocker,
        source: DecisionSource::Probed,
        warnings,
    }
}

/// The live entry point. Reads `FIRESTREAM_BUILD_STRATEGY` **before**
/// constructing a [`Probe`], so a forced strategy touches neither the
/// filesystem nor `PATH` — the shell's "escape hatch first, no probing"
/// guarantee, preserved.
pub fn decide_live(target_arch: Option<&str>, profile_default: Option<&str>) -> Decision {
    let raw = std::env::var(ENV_BUILD_STRATEGY).ok();
    let (ovr, warn) = StrategyOverride::parse_env(raw.as_deref());
    if let StrategyOverride::Force(s) = ovr {
        return Decision {
            strategy: s,
            // Deliberately not computed: no probing happened.
            blocker: None,
            source: DecisionSource::EnvOverride,
            warnings: warn.into_iter().collect(),
        };
    }
    let probe = Probe::detect();
    choose_strategy(&probe, target_arch, raw.as_deref(), profile_default)
}

/// Convenience: `decide_live` sourcing the profile default from a loaded
/// [`crate::profile::Profile`]. This is the call site that finally reads the
/// `build_strategy` block Phase 4 parsed but left unused.
pub fn decide_with_profile(
    target_arch: Option<&str>,
    profile: &crate::profile::Profile,
) -> Decision {
    decide_live(target_arch, Some(profile.build_strategy.default.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> Probe {
        Probe::default()
    }

    #[test]
    fn norm_arch_table() {
        assert_eq!(norm_arch("amd64"), "x86_64");
        assert_eq!(norm_arch("x64"), "x86_64");
        assert_eq!(norm_arch("x86_64"), "x86_64");
        assert_eq!(norm_arch("arm64"), "aarch64");
        assert_eq!(norm_arch("aarch64"), "aarch64");
        assert_eq!(norm_arch("riscv64"), "riscv64");
        assert_eq!(norm_arch(""), "");
    }

    #[test]
    fn clean_linux_host_builds_natively() {
        let p = linux();
        assert!(p.can_build_native(Some("x86_64")));
        assert_eq!(p.native_blocker(Some("x86_64")), None);
    }

    #[test]
    fn probe_default_is_a_fixture_not_the_host() {
        // The whole parity design rests on this: Probe::default() must be a
        // pure fixture, never a live read.
        let p = Probe::default();
        assert_eq!(p.uname_s, "Linux");
        assert_eq!(p.uname_m, "x86_64");
    }

    #[test]
    fn blocker_order_is_stable_when_all_apply() {
        let p = Probe {
            uname_s: "Darwin".into(),
            uname_m: "arm64".into(),
            nix_on_path: false,
            nix_store_present: false,
            in_container: true,
            ..Probe::default()
        };
        assert!(p.native_blocker(Some("x86_64")).unwrap().starts_with(
            "host is not Linux (uname -s = Darwin)"
        ));
    }

    #[test]
    fn env_override_beats_profile_default() {
        let p = linux();
        let d = choose_strategy(&p, Some("x86_64"), Some("native"), Some("docker"));
        assert_eq!(d.strategy, BuildStrategy::Native);
        assert_eq!(d.source, DecisionSource::EnvOverride);
    }

    #[test]
    fn profile_default_beats_probing() {
        let p = linux();
        let d = choose_strategy(&p, Some("x86_64"), None, Some("docker"));
        assert_eq!(d.strategy, BuildStrategy::Docker);
        assert_eq!(d.source, DecisionSource::ProfileDefault);
        assert_eq!(d.blocker, None, "the blocker still reports the truth");
    }

    #[test]
    fn unknown_env_value_warns_and_probes() {
        let p = linux();
        let d = choose_strategy(&p, Some("x86_64"), Some("frobnitz"), None);
        assert_eq!(d.strategy, BuildStrategy::Native);
        assert_eq!(d.source, DecisionSource::Probed);
        assert_eq!(d.warnings.len(), 1);
        assert!(d.warnings[0].contains("Unknown FIRESTREAM_BUILD_STRATEGY='frobnitz'"));
    }

    #[test]
    fn unknown_profile_value_warns_and_probes() {
        let p = linux();
        let d = choose_strategy(&p, Some("x86_64"), None, Some("sometimes"));
        assert_eq!(d.source, DecisionSource::Probed);
        assert!(d.warnings[0].contains("build_strategy.default='sometimes'"));
    }

    #[test]
    fn forced_native_still_reports_the_blocker() {
        let p = Probe {
            uname_s: "Darwin".into(),
            ..Probe::default()
        };
        let d = choose_strategy(&p, Some("x86_64"), Some("native"), None);
        assert_eq!(d.strategy, BuildStrategy::Native);
        assert!(d.blocker.is_some());
    }

    #[test]
    fn empty_target_arch_means_host_arch() {
        let p = Probe {
            uname_m: "aarch64".into(),
            ..Probe::default()
        };
        assert_eq!(p.native_blocker(None), None);
        assert_eq!(p.native_blocker(Some("")), None);
    }

    #[test]
    fn volume_and_platform_mirror_the_shell() {
        assert_eq!(nix_store_volume("x64"), "firestream-nix-store-x64");
        assert_eq!(nix_store_volume("amd64"), "firestream-nix-store-amd64");
        assert_eq!(docker_platform("arm64"), "linux/arm64");
        assert_eq!(docker_platform("riscv64"), "linux/riscv64");
    }
}
