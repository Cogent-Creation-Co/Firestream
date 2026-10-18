//! The CI **profile** — everything project-specific, as runtime data.
//!
//! ## What this replaces
//!
//! ConceptDB's `oxi-ci` carried its project knowledge in `src/defaults/mod.rs`
//! behind a `conceptdb` cargo feature: check attr lists, build attr lists with
//! an arch gate, builder image identity, an export-target classifier, a tier
//! classifier, and a passthrough env allowlist. The Firestream lift (Part A of
//! the plan) deleted that module and left 15 marked seams. This module fills
//! them, and the cargo feature does not come back: **there is no
//! project-specific Rust in this crate.**
//!
//! The deleted file is preserved verbatim at `docs/defaults-reference.rs.txt`.
//! It is the complete inventory of what the schema has to express, and its unit
//! tests are reproduced — from JSON alone, against a *synthetic non-Firestream
//! project* — by `tests/profile_fixtures.rs`. That test is the gate on the
//! invariant.
//!
//! ## The contract
//!
//! Nix typed options → JSON → Rust reader, mirroring the Helm chart contract
//! this repo already runs (documented in `CLAUDE.md`), file for file:
//!
//! | Charts                                          | CI profile                                   |
//! |-------------------------------------------------|----------------------------------------------|
//! | `bin/nix/firestream/charts/eval-chart.nix`      | `bin/nix/firestream/ci/eval-ci.nix`          |
//! | `.../charts/lib/types/*`                        | `.../ci/lib/types/*`                         |
//! | `.../charts/lib/to-chart-manifest.nix`          | `.../ci/lib/to-ci-manifest.nix`              |
//! | `chart-manifest.json` + `index.json`            | `ci-manifest.json`                           |
//! | `firestream-charts::{spec,reader}`              | `firestream_ci::profile::{spec,reader}`      |
//! | `FIRESTREAM_CHARTS_DIR` / `/opt/firestream/charts` | `FIRESTREAM_CI_PROFILE` / `/opt/firestream/ci` |
//!
//! ## The invariant
//!
//! `{system}` / `{arch}` (plus `{project}`, and `{leaf}` / `{stem}` inside an
//! export rule's `dest`) are the *entire* templating vocabulary. Conditional
//! attribute sets — ConceptDB's x86_64-only GPU package is the canonical case —
//! are expressed by **the Nix producer emitting a system-specific manifest**,
//! never by branching in Rust. Nothing in this module inspects a project name,
//! an attribute name, or an architecture to decide anything.
//!
//! ## Usage
//!
//! ```no_run
//! # fn main() -> Result<(), firestream_ci::profile::ProfileError> {
//! use firestream_ci::profile;
//!
//! // --profile <path> → FIRESTREAM_CI_PROFILE → ./ci-manifest.json
//! //                 → /opt/firestream/ci/ci-manifest.json
//! let p = profile::resolve(None)?;
//! let ctx = p.expand_ctx();
//! let verify = p.phase_attrs("verify", &ctx);
//! let (dest, kind) = p.resolve_export("airflow", &ctx);
//! # let _ = (verify, dest, kind);
//! # Ok(())
//! # }
//! ```

pub mod reader;
pub mod spec;

#[cfg(test)]
mod registry_parity;

pub use reader::{
    candidates, load, resolve, resolve_or_default, resolve_path, PROFILE_ENV, PROFILE_FILE,
    SYSTEM_PROFILE_DIR,
};
pub use spec::{
    attr_leaf, common_attr_prefix, Builder, BuildStrategy, EnvSentinel, ExpandCtx, ExportOutcome,
    ExportTarget, Matcher, PhaseSpec, Profile, ProfileError, Project, ShellTask, TierRule,
    BUILTIN_TASKS, KNOWN_ARTIFACT_KINDS, SCHEMA_VERSION,
};

// ───────────────────────────────────────────────────────────────────────────
// Wire-frame bridge
// ───────────────────────────────────────────────────────────────────────────
//
// `RunRequest` (proto/firestream/ci/v1/ci.proto) can carry an inline
// `CiProfile` so a caller driving agent mode over NDJSON stdin can ship the
// payload with the request instead of arranging for a file to exist on the
// far side. The proto message is a structural mirror of the JSON schema; the
// conversion below is mechanical.
//
// proto3 has no null, so "unset" is the zero value: an empty `image_name`
// leaves `Builder::image_name` empty, a zero `min_image_size_bytes` falls back
// to the 100 MiB default, and an absent optional string maps to `None`.

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

impl From<crate::wire::CiProfile> for Profile {
    fn from(w: crate::wire::CiProfile) -> Self {
        let defaults = Profile::default();
        Profile {
            schema_version: if w.schema_version == 0 {
                SCHEMA_VERSION
            } else {
                w.schema_version
            },
            project: w
                .project
                .map(|p| Project {
                    name: if p.name.is_empty() {
                        defaults.project.name.clone()
                    } else {
                        p.name
                    },
                    nix_system: p.nix_system,
                    arch: p.arch,
                    k8s_namespace_prefix: non_empty(p.k8s_namespace_prefix),
                })
                .unwrap_or_default(),
            phases: w
                .phases
                .into_iter()
                .map(|p| PhaseSpec {
                    name: p.name,
                    tier: tier_from_str(&p.tier),
                    depends_on: p.depends_on,
                    modes: p.modes,
                    attrs: p.attrs,
                    advisory_attrs: p.advisory_attrs,
                    // The proto `CiPhase` message predates the Phase-6 phase
                    // vocabulary (builtin/shell tasks, aggregation, export
                    // opt-in) and is deliberately NOT regenerated here: the
                    // wire path exists so an agent-mode caller can ship a
                    // *minimal* profile inline, and adding fields to the
                    // committed `src/wire/` requires a `--features codegen`
                    // regeneration. A wire-supplied phase therefore gets the
                    // conservative defaults: per-attr invocations, no builtin
                    // tasks, no export. Callers that need the full vocabulary
                    // pass a `ci-manifest.json` path (the normal route).
                    builtin_tasks: Vec::new(),
                    shell_tasks: Vec::new(),
                    aggregate: false,
                    export_artifacts: false,
                })
                .collect(),
            tier_rules: w
                .tier_rules
                .into_iter()
                .map(|r| TierRule {
                    matcher: r.r#match.map(matcher_from_wire).unwrap_or_default(),
                    tier: tier_from_str(&r.tier),
                })
                .collect(),
            tier_default: if w.tier_default.is_empty() {
                crate::pipeline::Tier::Required
            } else {
                tier_from_str(&w.tier_default)
            },
            export_targets: w
                .export_targets
                .into_iter()
                .map(|t| ExportTarget {
                    matcher: t.r#match.map(matcher_from_wire).unwrap_or_default(),
                    strip_prefix: non_empty(t.strip_prefix),
                    strip_suffix: non_empty(t.strip_suffix),
                    dest: t.dest,
                    kind: t.kind,
                })
                .collect(),
            export_default: w
                .export_default
                .map(|d| ExportOutcome {
                    dest: if d.dest.is_empty() {
                        "{leaf}".to_string()
                    } else {
                        d.dest
                    },
                    kind: if d.kind.is_empty() {
                        "binary".to_string()
                    } else {
                        d.kind
                    },
                })
                .unwrap_or_default(),
            passthrough_vars: w.passthrough_vars,
            devshell_sentinels: w
                .devshell_sentinels
                .into_iter()
                .map(|s| EnvSentinel {
                    name: s.name,
                    value: non_empty(s.value),
                })
                .collect(),
            builder: w
                .builder
                .map(|b| Builder {
                    image_name: b.image_name,
                    base_image: b.base_image,
                    min_image_size_bytes: if b.min_image_size_bytes == 0 {
                        defaults.builder.min_image_size_bytes
                    } else {
                        b.min_image_size_bytes
                    },
                    container_name_prefix: non_empty(b.container_name_prefix),
                })
                .unwrap_or_default(),
            build_strategy: w
                .build_strategy
                .map(|s| BuildStrategy {
                    default: if s.default.is_empty() {
                        "auto".to_string()
                    } else {
                        s.default
                    },
                    native_cache: s.native_cache,
                    docker_cache_volume: s.docker_cache_volume,
                })
                .unwrap_or_default(),
            // The proto `CiProfile` message has no container_registry field
            // (same rationale as the phase vocabulary above: the wire path
            // exists for a MINIMAL inline profile, and extending the committed
            // src/wire/ needs a --features codegen regeneration). A
            // wire-supplied profile therefore cannot drive `firestream-ci
            // build images`; it errors with EmptyContainerRegistry, which
            // names the fix. Callers that need it pass a ci-manifest.json.
            container_registry: Default::default(),
            provenance: w.provenance.into_iter().collect(),
        }
    }
}

fn matcher_from_wire(m: crate::wire::Matcher) -> Matcher {
    Matcher {
        equals: non_empty(m.equals),
        prefix: non_empty(m.prefix),
        suffix: non_empty(m.suffix),
        contains: non_empty(m.contains),
    }
}

/// Wire tier strings are lowercase; anything unrecognised (including the
/// proto3 empty default) is `required`, matching `tier_default`'s rationale —
/// an unknown value must not silently downgrade a gate.
fn tier_from_str(s: &str) -> crate::pipeline::Tier {
    match s {
        "advisory" => crate::pipeline::Tier::Advisory,
        _ => crate::pipeline::Tier::Required,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_profile_converts_and_validates() {
        let w = crate::wire::CiProfile {
            schema_version: 1,
            project: Some(crate::wire::CiProject {
                name: "acme".into(),
                nix_system: "x86_64-linux".into(),
                arch: "x86_64".into(),
                k8s_namespace_prefix: String::new(),
            }),
            phases: vec![crate::wire::CiPhase {
                name: "verify".into(),
                tier: "required".into(),
                attrs: vec!["checks.{system}.acme-fmt".into()],
                ..Default::default()
            }],
            tier_default: "required".into(),
            builder: Some(crate::wire::CiBuilder {
                image_name: "acme-builder".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let p: Profile = w.into();
        p.validate().unwrap();
        let ctx = p.expand_ctx();
        assert_eq!(
            p.phase_attrs("verify", &ctx),
            vec!["checks.x86_64-linux.acme-fmt".to_string()]
        );
        // Zero min_image_size_bytes on the wire falls back to the 100 MiB floor.
        assert_eq!(p.builder.min_image_size_bytes, 100 * 1024 * 1024);
        // Absent prefix derives from the project name.
        assert_eq!(p.project.k8s_namespace_prefix(), "acme-");
        assert_eq!(p.container_name_prefix(), "acme-ci-");
    }

    #[test]
    fn empty_wire_profile_is_the_default_profile() {
        let p: Profile = crate::wire::CiProfile::default().into();
        assert_eq!(p.schema_version, SCHEMA_VERSION);
        p.validate().unwrap();
    }
}
