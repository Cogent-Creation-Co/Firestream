//! Gate for the container -> Nix-package table in `src/config.rs`.
//!
//! # Why this file exists
//!
//! `bin/build/registry-cases.json` is the authority for
//! `container[:version] -> flake package name`. It already gates two
//! implementations:
//!
//! * `bin/build/_common.sh`'s `CONTAINER_REGISTRY` assoc-array, via
//!   `bin/build/test-registry-parity.sh` (`make test-registry-parity`);
//! * `firestream_ci::profile::Profile::resolve_package_name`, which reads the
//!   table as *data* out of `ci-manifest.json`
//!   (`bin/nix/firestream/ci/profile.nix` -> `ci.containerRegistry`).
//!
//! `BuildConfig::default_package_registry` was a third, hand-maintained copy,
//! and it **had drifted**: it mapped bare `airflow`/`kafka`/`spark`/`jupyterhub`
//! to the unsuffixed package names, where the authority says `airflow-3`,
//! `kafka-4`, `spark-4`, `jupyterhub-5`; and it was missing `odoo:15..18`
//! entirely. That copy is *not* dead code — `firestream-tui`'s
//! `backend/manifest.rs` calls `config.resolve_package_name(name, None)` to
//! populate `ContainerManifest::nix_package` — so the drift silently pointed
//! the TUI at different flake attributes than the shell build path.
//!
//! # Why a fourth copy at all, instead of delegating
//!
//! `nix-container-builder` is a **root workspace** member.
//! `firestream-ci` lives in `src/util`, a deliberately isolated Cargo workspace
//! (edition 2024, its own lockfile) — see the isolation rationale in
//! `src/util/firestream-ci/src/platform/mod.rs`. Adding a cargo dependency just
//! to read one table would pull ~15 duplicate transitive majors into the root
//! lock, which is exactly the coupling the isolation exists to prevent.
//!
//! So: keep the copy, but make it *checked* rather than *trusted*. This test
//! `include_str!`s the same JSON the other two harnesses use, so a drift is a
//! compile-and-test-time failure in the root workspace with no dependency edge.
//!
//! # Key-shape translation
//!
//! The bash array and the JSON `entries` spell "the family default" as a
//! trailing colon (`airflow:`). `BuildConfig`'s map spells it as the bare name
//! (`airflow`), because `resolve_package_name(c, None)` looks up `c` directly.
//! The translation is performed explicitly below and is the ONLY licensed
//! difference between the two tables.

use std::collections::BTreeMap;

use nix_container_builder::BuildConfig;

/// The authority. `include_str!` resolves relative to *this file*, so from
/// `src/lib/rust/nix-container-builder/tests/` the repo root is five levels up.
const REGISTRY_CASES_JSON: &str = include_str!("../../../../../bin/build/registry-cases.json");

#[derive(serde::Deserialize)]
struct RegistryCases {
    schema_version: u32,
    entries: BTreeMap<String, String>,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    container: String,
    /// Empty string means "no --version given".
    version: String,
    /// `null` means "unknown container: bash logs and returns 1".
    expect: Option<String>,
}

fn load() -> RegistryCases {
    serde_json::from_str(REGISTRY_CASES_JSON)
        .expect("bin/build/registry-cases.json is not valid JSON for this schema")
}

/// `airflow:` (authority spelling) -> `airflow` (BuildConfig spelling).
/// Everything else is passed through unchanged.
fn authority_key_to_config_key(key: &str) -> String {
    match key.strip_suffix(':') {
        Some(bare) => bare.to_string(),
        None => key.to_string(),
    }
}

#[test]
fn schema_version_is_the_one_this_test_understands() {
    assert_eq!(
        load().schema_version,
        1,
        "registry-cases.json bumped its schema_version; re-read the file before \
         trusting this test"
    );
}

#[test]
fn table_matches_authority_in_both_directions() {
    let authority = load().entries;
    let mine = BuildConfig::default().package_registry;

    let expected: BTreeMap<String, String> = authority
        .iter()
        .map(|(k, v)| (authority_key_to_config_key(k), v.clone()))
        .collect();

    // The translation must not collide: `airflow:` and `airflow` would both map
    // to `airflow`. The authority has no bare keys, so this holds; assert it so
    // a future bare key in the JSON is caught rather than silently shadowing.
    assert_eq!(
        expected.len(),
        authority.len(),
        "two authority keys collapsed to the same BuildConfig key under the \
         trailing-colon translation"
    );

    let mine: BTreeMap<String, String> = mine.into_iter().collect();

    let missing: Vec<_> = expected.keys().filter(|k| !mine.contains_key(*k)).collect();
    let extra: Vec<_> = mine.keys().filter(|k| !expected.contains_key(*k)).collect();
    let wrong: Vec<_> = expected
        .iter()
        .filter_map(|(k, want)| match mine.get(k) {
            Some(got) if got != want => Some(format!("{k}: have {got:?}, authority says {want:?}")),
            _ => None,
        })
        .collect();

    assert!(
        missing.is_empty() && extra.is_empty() && wrong.is_empty(),
        "BuildConfig::default_package_registry has drifted from \
         bin/build/registry-cases.json.\n  missing: {missing:?}\n  extra:   {extra:?}\n  \
         wrong:   {wrong:?}\nEdit registry-cases.json first, then mirror it into \
         src/config.rs; `make test-registry-parity` gates the bash side."
    );
}

#[test]
fn every_golden_vector_resolves_identically() {
    let cases = load().cases;
    let config = BuildConfig::default();

    assert!(cases.len() >= 26, "golden vectors disappeared: {}", cases.len());

    let mut failures = Vec::new();
    for case in &cases {
        // Bash passes `""` for "no version"; Rust passes `None`. Both mean
        // "try `<c>:<v>` (skipped), then the family default".
        let version = if case.version.is_empty() {
            None
        } else {
            Some(case.version.as_str())
        };
        let got = config.resolve_package_name(&case.container, version);
        if got.as_deref() != case.expect.as_deref() {
            failures.push(format!(
                "resolve_package_name({:?}, {:?}) = {:?}, expected {:?}",
                case.container, case.version, got, case.expect
            ));
        }
    }

    assert!(failures.is_empty(), "registry parity failures:\n  {}", failures.join("\n  "));
}

/// The specific vector the authority file calls "THE LANDMINE": `.#redis` is an
/// alias for redis-8 in the flake, but a bare `redis` on the build path must
/// still resolve to redis-7, or `make redis-7-start` enters a rebuild loop.
#[test]
fn bare_redis_is_redis_7_not_the_flake_alias() {
    let config = BuildConfig::default();
    assert_eq!(config.resolve_package_name("redis", None).as_deref(), Some("redis-7"));
    assert_eq!(config.resolve_package_name("redis", Some("8")).as_deref(), Some("redis-8"));
}
