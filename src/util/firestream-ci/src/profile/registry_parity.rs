//! The Rust half of the container-registry parity gate.
//!
//! Drives every vector in `bin/build/registry-cases.json` — the same file
//! `bin/build/test-registry-parity.sh` drives against
//! `bin/build/_common.sh::resolve_package_name`, and the same file
//! `bin/nix/firestream/ci/profile.nix` reads verbatim into
//! `ci.containerRegistry` — through [`Profile::resolve_package_name`].
//!
//! The registry itself is **loaded from the file's `entries` block**, exactly
//! as the Nix producer loads it. Nothing is transcribed into Rust; if the
//! table and the vectors ever disagree, this test is what says so.
//!
//! Sister to [`crate::platform::parity`]. Location of the vectors:
//! `CARGO_MANIFEST_DIR/../../../bin/build/registry-cases.json`, overridable
//! with `FIRESTREAM_REGISTRY_CASES` (for a Nix sandbox, where the repo root is
//! not three levels up).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::spec::{Profile, ProfileError};

#[derive(Debug, Deserialize)]
struct Cases {
    schema_version: u32,
    entries: BTreeMap<String, String>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    container: String,
    version: String,
    /// `null` means "resolution must fail".
    expect: Option<String>,
}

fn cases_path() -> PathBuf {
    if let Ok(p) = std::env::var("FIRESTREAM_REGISTRY_CASES") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../bin/build/registry-cases.json")
}

fn load() -> Cases {
    let path = cases_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden vectors at {} ({e}). Set FIRESTREAM_REGISTRY_CASES to override.",
            path.display()
        )
    });
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid registry-cases JSON: {e}", path.display()))
}

fn profile_from(entries: BTreeMap<String, String>) -> Profile {
    Profile {
        container_registry: entries,
        ..Profile::default()
    }
}

#[test]
fn golden_file_is_v1_and_non_trivial() {
    let c = load();
    assert_eq!(c.schema_version, 1);
    assert!(c.entries.len() >= 20, "registry shrank unexpectedly");
    assert!(c.cases.len() >= 20, "vector set shrank unexpectedly");
}

#[test]
fn every_vector_resolves_the_way_the_shell_does() {
    let c = load();
    let p = profile_from(c.entries);
    let mut failures = Vec::new();

    for case in &c.cases {
        let got = p.resolve_package_name(&case.container, &case.version);
        match (&case.expect, got) {
            (Some(want), Ok(got)) if *want == got => {}
            (Some(want), Ok(got)) => failures.push(format!(
                "resolve({}, {:?}) expected {want}, got {got}",
                case.container, case.version
            )),
            (Some(want), Err(e)) => failures.push(format!(
                "resolve({}, {:?}) expected {want}, got error: {e}",
                case.container, case.version
            )),
            (None, Err(_)) => {}
            (None, Ok(got)) => failures.push(format!(
                "resolve({}, {:?}) expected FAILURE, got {got}",
                case.container, case.version
            )),
        }
    }

    assert!(failures.is_empty(), "registry parity drift:\n  {}", failures.join("\n  "));
}

/// The landmine, asserted by name so a future reader trips over it in the test
/// output rather than in a rebuild loop.
#[test]
fn bare_redis_is_redis_7_not_the_flake_alias_redis_8() {
    let p = profile_from(load().entries);
    assert_eq!(p.resolve_package_name("redis", "").unwrap(), "redis-7");
    assert_eq!(p.resolve_package_name("redis", "8").unwrap(), "redis-8");
    assert_eq!(p.resolve_package_name("postgresql", "").unwrap(), "postgresql-17");
    assert_eq!(p.resolve_package_name("postgresql", "16").unwrap(), "postgresql-16");
}

/// An unrecognised VERSION falls back to the family default (shell behaviour);
/// an unrecognised CONTAINER is an error.
#[test]
fn unknown_version_falls_back_unknown_container_errors() {
    let p = profile_from(load().entries);
    assert_eq!(p.resolve_package_name("redis", "999").unwrap(), "redis-7");
    assert!(matches!(
        p.resolve_package_name("seaweedfs", ""),
        Err(ProfileError::UnknownContainer { .. })
    ));
}

/// A profile with no table blames the profile, not the container.
#[test]
fn empty_registry_is_its_own_error() {
    let p = Profile::default();
    assert!(matches!(
        p.resolve_package_name("redis", ""),
        Err(ProfileError::EmptyContainerRegistry)
    ));
}

#[test]
fn known_containers_is_the_deduped_sorted_family_list() {
    let p = profile_from(load().entries);
    let names = p.known_containers();
    assert!(names.contains(&"redis".to_string()));
    assert!(names.contains(&"odoo".to_string()));
    // Deduped: redis has 3 keys but one family name.
    assert_eq!(names.iter().filter(|n| *n == "redis").count(), 1);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}
