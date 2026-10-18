//! **The gate on "no project-specific Rust".**
//!
//! Phase 1 of the Firestream lift deleted ConceptDB's `src/defaults/mod.rs` —
//! the module that carried a project's CI knowledge as compiled-in Rust behind
//! a cargo feature. Its verbatim text (and, crucially, its unit tests) is
//! preserved at `docs/defaults-reference.rs.txt`.
//!
//! Every assertion in that file is reproduced below **from JSON alone**,
//! against a *synthetic second project* (`acmeforge`) that is not Firestream —
//! `tests/fixtures/other-project.json` and its `-aarch64` sibling. If the
//! schema could not express something the deleted module expressed in code,
//! one of these tests would be impossible to write, which is exactly the
//! failure mode the plan flags as the main design risk.
//!
//! The mapping, assertion for assertion:
//!
//! | `defaults-reference.rs.txt` test      | test below                              |
//! |---------------------------------------|-----------------------------------------|
//! | `verify_attrs_format`                 | `verify_attrs_format`                   |
//! | `verify_advisory_attrs_format`        | `verify_advisory_attrs_format`          |
//! | `build_attrs_format`                  | `build_attrs_format`                    |
//! | `build_attrs_aarch64_excludes_gpu`    | `build_attrs_aarch64_excludes_gpu`      |
//! | `build_export_target_image`           | `build_export_target_image`             |
//! | `build_export_target_sbom`            | `build_export_target_sbom`              |
//! | `build_export_target_binary`          | `build_export_target_binary`            |
//! | `build_export_target_wasm`            | `build_export_target_wasm`              |
//! | `tier_required_when_prefix_matches`   | `tier_required_when_prefix_matches`     |
//! | `tier_advisory_when_prefix_matches`   | `tier_advisory_when_prefix_matches`     |
//! | `tier_unknown_defaults_to_required`   | `tier_unknown_defaults_to_required`     |
//! | `passthrough_vars_contains_branch`    | `passthrough_vars_contains_branch`      |
//! | `BUILDER_IMAGE_NAME` / `NIX_BASE_IMAGE` / `MIN_BUILDER_IMAGE_SIZE_BYTES` (consts) | `builder_identity_and_size_floor` |
//!
//! Plus two tests with no reference counterpart, guarding schema properties
//! the reference got for free by being code: `profile_is_not_firestream` (the
//! fixture really does describe a different project) and
//! `every_build_attr_resolves_to_an_export_target`.

use std::path::{Path, PathBuf};

use firestream_ci::pipeline::Tier;
use firestream_ci::profile::{self, ExpandCtx, Profile};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The x86_64-linux manifest for the synthetic project.
fn acmeforge() -> Profile {
    profile::load(&fixture_path("other-project.json")).expect("x86_64 fixture must load + validate")
}

/// The aarch64-linux manifest for the same project — a *different document*,
/// which is how arch gating is expressed.
fn acmeforge_aarch64() -> Profile {
    profile::load(&fixture_path("other-project-aarch64.json"))
        .expect("aarch64 fixture must load + validate")
}

fn ctx(p: &Profile) -> ExpandCtx {
    p.expand_ctx()
}

// ───────────────────────────────────────────────────────────────────────────
// verify_attrs
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn verify_attrs_format() {
    let p = acmeforge();
    let a = p.phase_attrs("verify", &ctx(&p));
    assert!(a.contains(&"checks.x86_64-linux.required-rust-nextest".to_string()));
    assert!(a.contains(&"checks.x86_64-linux.required-ts-vitest".to_string()));
    assert!(a.contains(&"checks.x86_64-linux.required-ts-format".to_string()));
    assert!(a.contains(&"checks.x86_64-linux.required-hakari-verify".to_string()));
    assert!(a.contains(&"checks.x86_64-linux.required-proto-freshness".to_string()));
    assert_eq!(a.len(), 12);
}

#[test]
fn verify_advisory_attrs_format() {
    let p = acmeforge();
    let a = p.phase_advisory_attrs("verify", &ctx(&p));
    assert!(a.contains(&"checks.x86_64-linux.advisory-flux-models".to_string()));
    // Advisory tier so a failure never blocks the Required verdict.
    assert_eq!(p.tier_of("advisory-flux-models"), Tier::Advisory);
}

/// `{system}` expansion follows the *document*, not the host — swapping the
/// fixture swaps every attr path.
#[test]
fn verify_attrs_track_the_manifests_system() {
    let p = acmeforge_aarch64();
    let a = p.phase_attrs("verify", &ctx(&p));
    assert!(a.contains(&"checks.aarch64-linux.required-rust-fmt".to_string()));
    assert!(!a.iter().any(|s| s.contains("x86_64")));
}

// ───────────────────────────────────────────────────────────────────────────
// build_attrs (and the arch gate)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn build_attrs_format() {
    let p = acmeforge();
    let a = p.phase_attrs("build", &ctx(&p));
    assert!(a.contains(&"packages.x86_64-linux.acmeforge-server-linux-x86_64".to_string()));
    assert!(a.contains(&"packages.x86_64-linux.acmeforge-sbom".to_string()));
    // GPU server only on x86_64.
    assert!(a.contains(&"packages.x86_64-linux.acmeforge-server-linux-x86_64-gpu".to_string()));
    assert_eq!(a.len(), 9);
}

/// THE load-bearing test. The reference did this with `if arch == "x86_64"`
/// inside `build_attrs`. Here the exclusion is a property of *which document
/// the Nix producer emitted* — there is no branch to write, and no place in
/// `firestream_ci` where one could be written.
#[test]
fn build_attrs_aarch64_excludes_gpu() {
    let p = acmeforge_aarch64();
    let a = p.phase_attrs("build", &ctx(&p));
    assert!(
        !a.iter().any(|s| s.contains("-gpu")),
        "aarch64 manifest must not list the GPU target: {a:?}"
    );
    assert_eq!(a.len(), 8);
    assert!(a.contains(&"packages.aarch64-linux.acmeforge-server-linux-aarch64".to_string()));
}

// ───────────────────────────────────────────────────────────────────────────
// build_export_target — the four reference cases
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn build_export_target_image() {
    let p = acmeforge();
    let (dest, kind) = p.resolve_export("acmeforge-server-linux-x86_64", &ctx(&p));
    assert_eq!(dest, "acmeforge-server-linux-x86_64");
    assert_eq!(kind, "image");
}

#[test]
fn build_export_target_sbom() {
    let p = acmeforge();
    let (dest, kind) = p.resolve_export("acmeforge-sbom", &ctx(&p));
    assert_eq!(dest, "sbom");
    assert_eq!(kind, "sbom");
}

#[test]
fn build_export_target_binary() {
    let p = acmeforge();
    let (dest, kind) = p.resolve_export("acmeforge-cli", &ctx(&p));
    assert_eq!(dest, "acmeforge-cli-x86_64");
    assert_eq!(kind, "binary");
}

#[test]
fn build_export_target_wasm() {
    let p = acmeforge();
    let (dest, kind) = p.resolve_export("acmeforge-portal-wasm", &ctx(&p));
    assert_eq!(dest, "acmeforge-wasm/portal");
    assert_eq!(kind, "wasm");
}

/// The reference's multi-segment wasm names exercise `{stem}` beyond a single
/// word — `acmeforge-spreadsheet-editor-wasm` must not lose its interior
/// hyphens to an over-eager strip.
#[test]
fn build_export_target_wasm_multi_segment_stem() {
    let p = acmeforge();
    assert_eq!(
        p.resolve_export("acmeforge-spreadsheet-editor-wasm", &ctx(&p)).0,
        "acmeforge-wasm/spreadsheet-editor"
    );
    assert_eq!(
        p.resolve_export("acmeforge-core-dataflow-wasm", &ctx(&p)).0,
        "acmeforge-wasm/core-dataflow"
    );
}

/// The reference's `else` arm.
#[test]
fn build_export_target_fallback_is_identity_binary() {
    let p = acmeforge();
    let (dest, kind) = p.resolve_export("something-unclassified", &ctx(&p));
    assert_eq!(dest, "something-unclassified");
    assert_eq!(kind, "binary");
}

/// The `{arch}` in `dest` must follow the manifest, so the aarch64 document
/// produces `acmeforge-cli-aarch64` with no code change.
#[test]
fn build_export_target_binary_is_arch_templated() {
    let p = acmeforge_aarch64();
    assert_eq!(p.resolve_export("acmeforge-cli", &ctx(&p)).0, "acmeforge-cli-aarch64");
}

/// Rule ORDER is part of the contract: `acmeforge-sbom` must not be captured
/// by the earlier `-linux-` image rule, and the GPU attr (which ends in
/// `-gpu`, not `-wasm`) must still land as `image`.
#[test]
fn export_rules_are_ordered_first_match_wins() {
    let p = acmeforge();
    assert_eq!(p.resolve_export("acmeforge-sbom", &ctx(&p)).1, "sbom");
    assert_eq!(
        p.resolve_export("acmeforge-server-linux-x86_64-gpu", &ctx(&p)),
        ("acmeforge-server-linux-x86_64-gpu".to_string(), "image".to_string())
    );
}

// ───────────────────────────────────────────────────────────────────────────
// tier_classifier
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tier_required_when_prefix_matches() {
    let p = acmeforge();
    assert_eq!(p.tier_of("required-rust-fmt"), Tier::Required);
    // Full attr paths and bare leaves classify identically.
    assert_eq!(p.tier_of("checks.x86_64-linux.required-rust-fmt"), Tier::Required);
}

#[test]
fn tier_advisory_when_prefix_matches() {
    let p = acmeforge();
    assert_eq!(p.tier_of("advisory-something"), Tier::Advisory);
    assert_eq!(p.tier_of("checks.x86_64-linux.advisory-something"), Tier::Advisory);
}

#[test]
fn tier_unknown_defaults_to_required() {
    let p = acmeforge();
    assert_eq!(p.tier_of("rust-nextest"), Tier::Required);
}

/// The second tier mechanism: a phase's `advisory_attrs` forces Advisory even
/// when `tier_rules` would say otherwise. This is what lets a project whose
/// check names carry NO tier convention (Firestream's `firestream-*`) express
/// the same thing the reference expressed with a naming convention.
#[test]
fn phase_advisory_attrs_override_tier_rules() {
    let p: Profile = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "project": { "name": "acmeforge", "nix_system": "x86_64-linux", "arch": "x86_64" },
        "phases": [ { "name": "verify",
                      "attrs": ["checks.{system}.plain-a"],
                      "advisory_attrs": ["checks.{system}.plain-b"] } ],
        "tier_rules": [],
        "tier_default": "required"
    }))
    .unwrap();
    let c = ctx(&p);
    assert_eq!(p.tier_of_in_phase("verify", "checks.x86_64-linux.plain-a", &c), Tier::Required);
    assert_eq!(p.tier_of_in_phase("verify", "checks.x86_64-linux.plain-b", &c), Tier::Advisory);
}

/// An advisory PHASE makes every attr in it advisory: a required attr whose
/// failure cannot fail the run is a contradiction, and mislabels its log file.
#[test]
fn advisory_phase_downgrades_its_attrs() {
    let p = acmeforge();
    let c = ctx(&p);
    // `acmeforge-sbom` classifies as Required under `tier_default`…
    assert_eq!(p.tier_of("acmeforge-sbom"), Tier::Required);
    // …and is Required inside the required `build` phase…
    assert_eq!(
        p.tier_of_in_phase("build", "packages.x86_64-linux.acmeforge-sbom", &c),
        Tier::Required
    );
    // …but Advisory inside the advisory `attest` phase.
    assert_eq!(
        p.tier_of_in_phase("attest", "packages.x86_64-linux.acmeforge-sbom", &c),
        Tier::Advisory
    );
}

// ───────────────────────────────────────────────────────────────────────────
// passthrough / builder / sentinels
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn passthrough_vars_contains_branch() {
    let p = acmeforge();
    assert!(p.passthrough_vars.contains(&"BRANCH".to_string()));
    assert!(p.passthrough_vars.contains(&"OTEL_EXPORTER_OTLP_ENDPOINT".to_string()));
    // The reference's list was 22 entries.
    assert_eq!(p.passthrough_vars.len(), 22);
}

#[test]
fn builder_identity_and_size_floor() {
    let p = acmeforge();
    assert_eq!(p.builder.image_name, "acmeforge-builder");
    assert_eq!(p.builder.base_image, "acmeforge-nix-base:2.34.1");
    assert_eq!(p.builder.min_image_size_bytes, 100 * 1024 * 1024);
    // Derived, not stored.
    assert_eq!(p.container_name_prefix(), "acmeforge-ci-");
    assert_eq!(p.project.k8s_namespace_prefix(), "acmeforge-");
}

#[test]
fn devshell_sentinels_cover_presence_and_exact_value() {
    let p = acmeforge();
    let in_shell = |k: &str| match k {
        "IN_NIX_SHELL" => Some("impure".to_string()),
        _ => None,
    };
    let marker_on = |k: &str| match k {
        "ACMEFORGE_DEVSHELL" => Some("1".to_string()),
        _ => None,
    };
    let marker_off = |k: &str| match k {
        "ACMEFORGE_DEVSHELL" => Some("0".to_string()),
        _ => None,
    };
    let bare = |_: &str| None;
    assert!(p.in_devshell(&in_shell));
    assert!(p.in_devshell(&marker_on));
    assert!(!p.in_devshell(&marker_off));
    assert!(!p.in_devshell(&bare));
}

#[test]
fn build_strategy_docker_volume_is_arch_templated() {
    let p = acmeforge();
    assert_eq!(
        p.expand(&p.build_strategy.docker_cache_volume, &ctx(&p)),
        "acmeforge-nix-store-x86_64"
    );
    let q = acmeforge_aarch64();
    assert_eq!(
        q.expand(&q.build_strategy.docker_cache_volume, &ctx(&q)),
        "acmeforge-nix-store-aarch64"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Schema-level properties with no reference counterpart
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn phase_dag_matches_the_reference_pipeline_shape() {
    let p = acmeforge();
    let release: Vec<&str> = p
        .phase_order("release")
        .unwrap()
        .iter()
        .map(|x| x.name.as_str())
        .collect();
    assert_eq!(release, vec!["tidy", "verify", "build", "attest"]);

    // `build` and `attest` are release-only.
    let check: Vec<&str> = p
        .phase_order("check")
        .unwrap()
        .iter()
        .map(|x| x.name.as_str())
        .collect();
    assert_eq!(check, vec!["tidy", "verify"]);

    assert_eq!(p.phase("tidy").unwrap().tier, Tier::Advisory);
    assert_eq!(p.phase("verify").unwrap().tier, Tier::Required);
    assert_eq!(p.phase("build").unwrap().tier, Tier::Required);
    assert_eq!(p.phase("attest").unwrap().tier, Tier::Advisory);
}

/// Nothing in the fixture mentions Firestream, and nothing in `firestream_ci`
/// needed to know it wouldn't. This is the assertion the deleted `conceptdb`
/// cargo feature used to make structurally.
#[test]
fn profile_is_not_firestream() {
    // Check the DATA, not the file text — the fixture's `_comment` blocks
    // explain the contract and legitimately name Firestream. Round-tripping
    // through `Profile` drops every non-schema key, which is exactly the view
    // the tool acts on.
    let p = acmeforge();
    let data = serde_json::to_string(&p).unwrap().to_lowercase();
    assert!(!data.contains("firestream"), "fixture must describe a different project");
    assert!(!data.contains("conceptdb"), "fixture must be synthetic, not the original repo");
    assert_eq!(p.project.name, "acmeforge");
}

/// Every `build` attr must resolve to some export target — the reference had
/// a total function (`else` arm), and so must the schema.
#[test]
fn every_build_attr_resolves_to_an_export_target() {
    for p in [acmeforge(), acmeforge_aarch64()] {
        let c = ctx(&p);
        for attr in p.phase_attrs("build", &c) {
            let leaf = profile::attr_leaf(&attr);
            let (dest, kind) = p.resolve_export(leaf, &c);
            assert!(!dest.is_empty(), "{attr}: empty dest");
            assert!(
                profile::KNOWN_ARTIFACT_KINDS.contains(&kind.as_str()),
                "{attr}: unknown kind {kind}"
            );
            assert!(!dest.contains('{'), "{attr}: unexpanded placeholder in {dest}");
        }
    }
}

/// A second project's profile must not need any Firestream file to exist:
/// loading it from an arbitrary path, with no env and no `./ci-manifest.json`
/// in cwd, has to work.
#[test]
fn fixture_loads_from_an_arbitrary_path_with_no_ambient_state() {
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("somewhere-else.json");
    std::fs::copy(fixture_path("other-project.json"), &dst).unwrap();
    let p = profile::resolve(Some(&dst)).unwrap();
    assert_eq!(p.project.name, "acmeforge");
    assert_eq!(p.schema_version, profile::SCHEMA_VERSION);
}
