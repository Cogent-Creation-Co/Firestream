//! CI gate: confirms the lift-fixture — a synthetic external consumer of
//! `firestream-ci` — compiles against the crate's ordinary default build.
//!
//! ConceptDB's version of this test asserted `--no-default-features`
//! behaviour, because project specifics sat behind a `conceptdb` cargo
//! feature. That feature no longer exists: the Firestream lift deleted it and
//! project specifics become runtime `ci-manifest.json` data (Phase 4). So the
//! invariant is now stronger and simpler — the core API is project-agnostic
//! *by construction*, with nothing to opt out of. Any compiled-in project
//! assumption that an external consumer cannot satisfy fails this test.

#[test]
fn lift_fixture_builds_against_default_features() {
    let output = std::process::Command::new("cargo")
        .args(["check", "--manifest-path", "tests/lift-fixture/Cargo.toml"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo check failed to spawn");
    assert!(
        output.status.success(),
        "lift-fixture failed to build — project-specific coupling leaked into the core API.\n\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Guard the invariant itself: the crate must declare no project-specific
/// cargo feature. `codegen` (protoc regeneration) is the only permitted one,
/// and there must be no `default = [...]` feature set.
#[test]
fn crate_declares_no_project_specific_feature() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();

    // Extract the `[features]` table body (up to the next `[section]`).
    let start = manifest
        .find("\n[features]\n")
        .expect("Cargo.toml has a [features] section")
        + "\n[features]\n".len();
    let body = &manifest[start..];
    let end = body.find("\n[").unwrap_or(body.len());
    let body = &body[..end];

    let declared: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split('=').next())
        .map(str::trim)
        .collect();

    assert_eq!(
        declared,
        vec!["codegen"],
        "firestream-ci declares cargo features {declared:?}; only `codegen` is \
         permitted. Project specifics belong in ci-manifest.json (Phase 4), not \
         in a cargo feature — and there must be no `default` feature set."
    );
}
