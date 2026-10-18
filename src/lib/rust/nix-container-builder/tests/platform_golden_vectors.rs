//! Gate for this crate's copy of the native-vs-Docker predicate
//! (`src/platform.rs`) against the shared golden vectors.
//!
//! # Why
//!
//! `bin/build/strategy-cases.json` is the authority for "should this Nix build
//! run natively on the host store, or inside a `nixos/nix` container?". Two
//! harnesses already consume it:
//!
//! * `bin/build/test-strategy-parity.sh` -> `bin/build/strategy.sh` (bash);
//! * `src/util/firestream-ci/src/platform/parity.rs` ->
//!   `firestream_ci::platform::Probe` (rust, `src/util` workspace).
//!
//! `nix_container_builder::platform` is a third implementation. It is
//! **scheduled to be deleted** in favour of delegating to
//! `firestream_ci::platform` (see the delegation map in `src/platform.rs`), but
//! that merge is blocked on the Phase 7 strangler observation checklist and
//! would couple the root workspace to the isolated `src/util` workspace. Until
//! then, this test makes the third copy *checked* rather than *trusted*, at the
//! cost of one `include_str!` and no cargo dependency edge whatsoever.
//!
//! # The filter, and why each exclusion exists
//!
//! This crate's predicate is strictly **narrower** than the authority's. It is
//! not a bug-for-bug mirror and this test does not pretend otherwise; it
//! asserts agreement only on the subset this crate can express, and names every
//! excluded axis so the eventual delegation is mechanical:
//!
//! | Excluded | Why |
//! |---|---|
//! | `env.FIRESTREAM_BUILD_STRATEGY` | This crate has no escape-hatch env var at all. Rung 1 of the shared precedence ladder is simply absent here; `--native`/`--docker` on the CLI are the only overrides. |
//! | `profile_default` | This crate never reads `ci-manifest.json`. Rung 4 absent. |
//! | cross-arch `target_arch` | `can_build_native()` takes no target argument, so it cannot see blocker #2. |
//! | `uname_m` outside x86_64/aarch64 | `Architecture::detect` errors on anything else; the authority passes it through. |
//! | `uname_s` outside Linux/Darwin | `Platform::detect` errors; the authority treats every non-Linux the same. |
//! | `nix_store_present != nix_on_path` | **A real semantic gap.** `check_nix_available()` only does `which nix`; the authority additionally requires `/nix/store` to be a directory (blocker #4). A host with the Nix client but no store is `native` here and `docker` there. Lifting this is a one-line change at delegation time. |
//!
//! Container-route env vars (`KUBERNETES_SERVICE_HOST`, `CONTAINER`,
//! `container`) are *not* excluded: they are folded into `in_container` below,
//! mirroring `is_running_in_container()`. Adding this test immediately found a
//! real defect there — the crate matched `docker`/`kubepods` in
//! `/proc/1/cgroup` but not `containerd`, so a k3s/containerd pod read as "on
//! the host". Fixed in `src/platform.rs`; the vector
//! `blocker-5-container-route-cgroup-containerd` now gates it.

use std::collections::BTreeMap;

use nix_container_builder::platform::{Architecture, Platform, PlatformInfo};

/// `include_str!` resolves relative to this file: `tests/` -> repo root is five up.
const STRATEGY_CASES_JSON: &str = include_str!("../../../../../bin/build/strategy-cases.json");

#[derive(serde::Deserialize)]
struct StrategyCases {
    schema_version: u32,
    arch_cases: Vec<ArchCase>,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct ArchCase {
    input: String,
    norm: String,
    nix_volume: String,
    docker_platform: String,
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    probe: Probe,
    target_arch: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    profile_default: Option<String>,
    expect: Expect,
}

#[derive(serde::Deserialize)]
struct Probe {
    uname_s: String,
    uname_m: String,
    nix_on_path: bool,
    nix_store_present: bool,
    dockerenv_file: bool,
    cgroup: Option<String>,
}

#[derive(serde::Deserialize)]
struct Expect {
    /// `null` = native is possible.
    blocker: Option<String>,
}

fn load() -> StrategyCases {
    serde_json::from_str(STRATEGY_CASES_JSON)
        .expect("bin/build/strategy-cases.json is not valid JSON for this schema")
}

fn norm_arch(a: &str) -> &str {
    match a {
        "amd64" | "x64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => other,
    }
}

fn arch_of(a: &str) -> Option<Architecture> {
    match norm_arch(a) {
        "x86_64" => Some(Architecture::X86_64),
        "aarch64" => Some(Architecture::Aarch64),
        _ => None,
    }
}

fn platform_of(s: &str) -> Option<Platform> {
    match s {
        "Linux" => Some(Platform::Linux),
        "Darwin" => Some(Platform::Darwin),
        _ => None,
    }
}

/// Mirror of `platform::is_running_in_container`, computed from case data so
/// the test never touches process env or the real filesystem.
fn in_container(probe: &Probe, env: &BTreeMap<String, String>) -> bool {
    if probe.dockerenv_file {
        return true;
    }
    for var in ["KUBERNETES_SERVICE_HOST", "CONTAINER", "container"] {
        if env.get(var).is_some_and(|v| !v.is_empty()) {
            return true;
        }
    }
    matches!(&probe.cgroup, Some(c) if c.contains("docker")
        || c.contains("kubepods")
        || c.contains("containerd"))
}

#[test]
fn schema_version_is_the_one_this_test_understands() {
    assert_eq!(load().schema_version, 1);
}

#[test]
fn arch_helpers_match_the_authority() {
    let mut checked = 0;
    for case in load().arch_cases {
        // Excluded inputs:
        //   * riscv64 — this crate's `Architecture` enum cannot represent it.
        //   * x64 — the authority's `docker_arch_alias` matches on the RAW arch
        //     and passes `x64` straight through (`linux/x64`,
        //     `firestream-nix-store-x64`), preserved bug-for-bug from the shell
        //     `get_nix_volume`. This crate folds `x64` into `X86_64` and would
        //     answer `linux/amd64`. Nothing in this crate parses `x64` (it goes
        //     through `std::env::consts::ARCH`), so the divergence is
        //     unreachable here — but it is real and is why delegation must take
        //     the authority's string helpers, not just its enum.
        if !matches!(case.input.as_str(), "amd64" | "x86_64" | "arm64" | "aarch64") {
            continue;
        }
        let Some(arch) = arch_of(&case.input) else {
            continue;
        };
        assert_eq!(
            norm_arch(&case.input),
            case.norm,
            "arch normalisation drifted for {:?}",
            case.input
        );
        assert_eq!(
            arch.docker_platform(),
            case.docker_platform,
            "Architecture::docker_platform drifted for {:?}",
            case.input
        );
        let info = fixture(Platform::Linux, arch, true, false);
        assert_eq!(
            info.default_nix_store_volume(),
            case.nix_volume,
            "PlatformInfo::default_nix_store_volume drifted for {:?}",
            case.input
        );
        checked += 1;
    }
    assert!(checked >= 4, "arch_cases shrank unexpectedly: {checked}");
}

fn fixture(
    platform: Platform,
    arch: Architecture,
    nix_available: bool,
    in_container: bool,
) -> PlatformInfo {
    PlatformInfo {
        platform,
        arch,
        nix_available,
        // Irrelevant to can_build_native(); see recommended_strategy()'s docs
        // for the one place docker_available matters and why that difference is
        // deliberate.
        docker_available: true,
        in_container,
    }
}

#[test]
fn can_build_native_matches_the_authority_on_the_expressible_subset() {
    let cases = load().cases;
    let mut checked = 0;
    let mut failures = Vec::new();

    for case in &cases {
        // ── the documented filter (see module docs) ──────────────────────
        if case.env.contains_key("FIRESTREAM_BUILD_STRATEGY") {
            continue;
        }
        if case.profile_default.as_deref().is_some_and(|d| d != "auto") {
            continue;
        }
        let Some(platform) = platform_of(&case.probe.uname_s) else {
            continue;
        };
        let Some(arch) = arch_of(&case.probe.uname_m) else {
            continue;
        };
        let host = norm_arch(&case.probe.uname_m);
        let target = if case.target_arch.is_empty() {
            host
        } else {
            norm_arch(&case.target_arch)
        };
        if target != host {
            continue; // arch-blind here
        }
        if case.probe.nix_store_present != case.probe.nix_on_path {
            continue; // the /nix/store gap
        }
        // ─────────────────────────────────────────────────────────────────

        let info = fixture(
            platform,
            arch,
            case.probe.nix_on_path,
            in_container(&case.probe, &case.env),
        );
        let want_native = case.expect.blocker.is_none();
        if info.can_build_native() != want_native {
            failures.push(format!(
                "{}: can_build_native() = {}, authority says {} (blocker {:?})",
                case.name,
                info.can_build_native(),
                want_native,
                case.expect.blocker
            ));
        }
        checked += 1;
    }

    assert!(
        checked >= 8,
        "the filter excluded too much to be a meaningful gate ({checked} of {} cases); \
         did the case shapes change?",
        cases.len()
    );
    assert!(
        failures.is_empty(),
        "nix_container_builder::platform has drifted from bin/build/strategy-cases.json \
         on the subset it claims to implement:\n  {}",
        failures.join("\n  ")
    );
}
