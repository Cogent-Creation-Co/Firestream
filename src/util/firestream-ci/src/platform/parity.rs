//! The Rust half of the strategy parity gate.
//!
//! Drives every vector in `bin/build/strategy-cases.json` — the same file
//! `bin/build/test-strategy-parity.sh` drives — through [`Probe`] fixtures.
//! Nothing here touches the live host: a case's `probe` block *is* the `Probe`,
//! and its `env` block is passed as function arguments rather than exported, so
//! the tests are order-independent and safe under a parallel test runner.
//!
//! Divergence from the shell shows up as a failure on one side or the other,
//! because both sides read the same expectations out of the same file.
//!
//! Location of the vectors: `CARGO_MANIFEST_DIR/../../../bin/build/strategy-cases.json`,
//! overridable with `FIRESTREAM_STRATEGY_CASES` (for a Nix sandbox, where the
//! repo root is not three levels up).

use std::path::PathBuf;

use serde::Deserialize;

use super::*;

#[derive(Debug, Deserialize)]
struct Cases {
    schema_version: u32,
    arch_cases: Vec<ArchCase>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct ArchCase {
    input: String,
    norm: String,
    nix_volume: String,
    docker_platform: String,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    probe: ProbeSpec,
    target_arch: String,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    profile_default: Option<String>,
    expect: Expect,
    /// Present only where the shell, being profile-blind, must legitimately
    /// disagree. Unused on this side — recorded so the file documents both.
    #[serde(default)]
    expect_shell: Option<Expect>,
}

#[derive(Debug, Deserialize)]
struct ProbeSpec {
    uname_s: String,
    uname_m: String,
    nix_on_path: bool,
    nix_store_present: bool,
    dockerenv_file: bool,
    cgroup: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Expect {
    #[serde(default)]
    blocker: Option<String>,
    strategy: String,
    #[serde(default)]
    warns: bool,
}

impl ProbeSpec {
    /// Fold the raw filesystem/env conditions into a `Probe` exactly the way
    /// `fs_in_container` does: `/.dockerenv`, then the three env vars, then a
    /// `/proc/1/cgroup` pattern match.
    fn to_probe(&self, env: &std::collections::BTreeMap<String, String>) -> Probe {
        let cgroup_hit = self
            .cgroup
            .as_deref()
            .map(|c| c.contains("docker") || c.contains("kubepods") || c.contains("containerd"))
            .unwrap_or(false);
        let env_hit = ["KUBERNETES_SERVICE_HOST", "CONTAINER", "container"]
            .iter()
            .any(|k| env.get(*k).map(|v| !v.is_empty()).unwrap_or(false));

        Probe {
            uname_s: self.uname_s.clone(),
            uname_m: self.uname_m.clone(),
            nix_on_path: self.nix_on_path,
            nix_store_present: self.nix_store_present,
            docker_on_path: true,
            docker_daemon_up: true,
            in_container: self.dockerenv_file || env_hit || cgroup_hit,
            dockerenv_file: self.dockerenv_file,
            cloudbuild_env: false,
        }
    }
}

fn cases_path() -> PathBuf {
    if let Ok(p) = std::env::var("FIRESTREAM_STRATEGY_CASES") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../bin/build/strategy-cases.json")
}

fn load() -> Cases {
    let path = cases_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden vectors at {} ({e}). Set FIRESTREAM_STRATEGY_CASES to override.",
            path.display()
        )
    });
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid strategy-cases JSON: {e}", path.display()))
}

#[test]
fn golden_file_is_the_expected_schema() {
    let c = load();
    assert_eq!(c.schema_version, 1);
    assert!(!c.cases.is_empty());
    assert!(!c.arch_cases.is_empty());
}

#[test]
fn arch_helpers_match_the_golden_vectors() {
    let c = load();
    let mut failures = Vec::new();
    for a in &c.arch_cases {
        if norm_arch(&a.input) != a.norm {
            failures.push(format!(
                "{}: norm_arch -> {:?}, expected {:?}",
                a.input,
                norm_arch(&a.input),
                a.norm
            ));
        }
        if nix_store_volume(&a.input) != a.nix_volume {
            failures.push(format!(
                "{}: nix_store_volume -> {:?}, expected {:?}",
                a.input,
                nix_store_volume(&a.input),
                a.nix_volume
            ));
        }
        if docker_platform(&a.input) != a.docker_platform {
            failures.push(format!(
                "{}: docker_platform -> {:?}, expected {:?}",
                a.input,
                docker_platform(&a.input),
                a.docker_platform
            ));
        }
    }
    assert!(failures.is_empty(), "arch parity divergence:\n{}", failures.join("\n"));
}

/// The CI profile's `build_strategy.docker_cache_volume` template must land on
/// the SAME volume name `get_nix_volume` produces, for every golden arch.
///
/// This is the assertion that would have caught the Phase 6 hand-off bug: the
/// `ci-docker` fallback expanded `{arch}` with the *normalised* arch
/// (`x86_64`) and would have mounted `firestream-nix-store-x86_64` — an empty
/// volume — instead of the `firestream-nix-store-amd64` the shell path has
/// been warming all along. Cold cache, no error, hours of rebuild.
#[test]
fn profile_docker_cache_volume_template_matches_get_nix_volume() {
    const TEMPLATE: &str = "firestream-nix-store-{arch}";
    let c = load();
    let mut failures = Vec::new();
    for a in &c.arch_cases {
        // The RAW arch, exactly as `get_nix_volume` receives it. Callers must
        // NOT pre-normalise — see the `x64` case below.
        let got = docker_cache_volume(TEMPLATE, &a.input);
        if got != a.nix_volume {
            failures.push(format!(
                "{}: docker_cache_volume -> {got:?}, expected {:?}",
                a.input, a.nix_volume
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "docker cache volume divergence:\n{}",
        failures.join("\n")
    );
    assert_eq!(docker_cache_volume("", "x86_64"), "");
    assert_eq!(docker_cache_volume("fixed-name", "x86_64"), "fixed-name");

    // Pre-normalising is the trap, and it is only visible on the odd aliases:
    // `x64` normalises to `x86_64`, and the two produce DIFFERENT volumes.
    // Documented as an assertion so nobody "tidies" a norm_arch() into the
    // call chain.
    assert_eq!(docker_cache_volume(TEMPLATE, "x64"), "firestream-nix-store-x64");
    assert_ne!(
        docker_cache_volume(TEMPLATE, "x64"),
        docker_cache_volume(TEMPLATE, &norm_arch("x64"))
    );
}

#[test]
fn strategy_decision_matches_the_golden_vectors() {
    let c = load();
    let mut failures = Vec::new();

    for case in &c.cases {
        let probe = case.probe.to_probe(&case.env);
        let target = Some(case.target_arch.as_str());
        let env_strategy = case.env.get(ENV_BUILD_STRATEGY).map(|s| s.as_str());
        let profile_default = case.profile_default.as_deref();

        let d = choose_strategy(&probe, target, env_strategy, profile_default);

        if d.blocker != case.expect.blocker {
            failures.push(format!(
                "{}: blocker\n    expected {:?}\n    actual   {:?}",
                case.name, case.expect.blocker, d.blocker
            ));
        }
        if d.strategy.label() != case.expect.strategy {
            failures.push(format!(
                "{}: strategy expected {:?}, actual {:?}",
                case.name,
                case.expect.strategy,
                d.strategy.label()
            ));
        }
        let warned = !d.warnings.is_empty();
        if warned != case.expect.warns {
            failures.push(format!(
                "{}: warns expected {}, actual {} ({:?})",
                case.name, case.expect.warns, warned, d.warnings
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "strategy parity divergence ({} of {} cases):\n{}",
        failures.len(),
        c.cases.len(),
        failures.join("\n")
    );
}

/// The `expect_shell` field is the one place the two implementations are
/// *allowed* to disagree, so its use is fenced: it may appear only on a case
/// whose `profile_default` is set to something other than `auto`, which is the
/// only reason a profile-blind shell can legitimately differ. Both harnesses
/// enforce this, so the field cannot be repurposed to bless real drift.
#[test]
fn expect_shell_is_only_used_for_profile_driven_divergence() {
    let c = load();
    for case in &c.cases {
        if case.expect_shell.is_some() {
            let p = case.profile_default.as_deref().unwrap_or("");
            assert!(
                !p.is_empty() && p != "auto",
                "{}: expect_shell is only legal when profile_default is set and != auto \
                 (got {:?}). The shell and Rust must otherwise agree exactly.",
                case.name,
                case.profile_default
            );
        }
    }
}

/// Guards against the vector file silently losing coverage of a blocker.
#[test]
fn every_blocker_string_is_covered_by_a_vector() {
    let c = load();
    let seen: Vec<&str> = c
        .cases
        .iter()
        .filter_map(|x| x.expect.blocker.as_deref())
        .collect();
    for needle in [
        "image derivations are gated behind isLinux",
        "cross-arch target (",
        "nix not found on PATH",
        "/nix/store does not exist on this host",
        "running inside a container",
    ] {
        assert!(
            seen.iter().any(|s| s.contains(needle)),
            "no golden vector covers the blocker containing {needle:?}"
        );
    }
}

/// Guards against the vector file losing coverage of a `fs_in_container` route.
#[test]
fn every_container_route_is_covered_by_a_vector() {
    let c = load();
    let containerised = |name: &str| c.cases.iter().find(|x| x.name == name);
    for name in [
        "blocker-5-container-route-dockerenv",
        "blocker-5-container-route-kubernetes-service-host",
        "blocker-5-container-route-CONTAINER-upper",
        "blocker-5-container-route-container-lower",
        "blocker-5-container-route-cgroup-docker",
        "blocker-5-container-route-cgroup-kubepods",
        "blocker-5-container-route-cgroup-containerd",
    ] {
        let case = containerised(name)
            .unwrap_or_else(|| panic!("golden vectors lost the container route case {name:?}"));
        assert_eq!(
            case.expect.blocker.as_deref(),
            Some("running inside a container"),
            "{name} no longer asserts the container blocker"
        );
    }
}
