//! v1 schema types for `ci-manifest.json` — the CI profile payload.
//!
//! These types mirror the JSON shape emitted by
//! `bin/nix/firestream/ci/lib/to-ci-manifest.nix`, exactly as
//! `src/lib/rust/firestream-charts/src/spec.rs` mirrors
//! `bin/nix/firestream/charts/lib/to-chart-manifest.nix`. The contract is the
//! same one `CLAUDE.md` documents for Helm charts: **Nix typed options → JSON →
//! Rust reader**, with the Nix side as the only place project knowledge lives.
//!
//! ## Provenance
//!
//! Every field here exists because ConceptDB's deleted `src/defaults/mod.rs`
//! expressed it as compiled-in Rust behind a cargo feature. That file is
//! preserved verbatim at `docs/defaults-reference.rs.txt` and is the complete
//! inventory this schema must express. The mapping:
//!
//! | `defaults-reference.rs.txt`     | schema home                          |
//! |---------------------------------|--------------------------------------|
//! | `BUILDER_IMAGE_NAME`            | `builder.image_name`                 |
//! | `NIX_BASE_IMAGE`                | `builder.base_image`                 |
//! | `MIN_BUILDER_IMAGE_SIZE_BYTES`  | `builder.min_image_size_bytes`       |
//! | `CI_PASSTHROUGH_VARS`           | `passthrough_vars`                   |
//! | `verify_attrs`                  | `phases[verify].attrs`               |
//! | `verify_advisory_attrs`         | `phases[verify].advisory_attrs`      |
//! | `build_attrs`                   | `phases[build].attrs`                |
//! | `build_attrs` arch gate (`-gpu`)| Nix emits a system-specific manifest |
//! | `build_export_target`           | `export_targets` + `export_default`  |
//! | `tier_classifier`               | `tier_rules` + `tier_default`        |
//!
//! ## Shape rules
//!
//! - JSON keys are **snake_case** (unlike the chart manifest's camelCase): the
//!   plan's schema sketch and the `jq -e '.schema_version == 1'` acceptance
//!   check both spell them that way, and `to-ci-manifest.nix` maps its
//!   camelCase Nix option names onto them explicitly — the same explicit
//!   mapping `to-chart-manifest.nix` performs. Rust therefore needs no
//!   `rename_all`.
//! - `schema_version` is an **integer**, not a string.
//! - Every collection carries `#[serde(default)]` so a missing key is an empty
//!   collection rather than a parse error.
//! - Optional scalars are `Option<T>` with `skip_serializing_if` so a
//!   round-trip does not invent nulls.
//!
//! ## The zero-project-reasoning invariant
//!
//! Nothing in this module branches on a project name, an attribute name, or an
//! architecture. Everything it does is: substitute `{placeholders}`, test a
//! string against a declarative [`Matcher`], and look a value up. Conditional
//! attribute sets (ConceptDB's x86_64-only GPU target) are expressed by the
//! **Nix producer emitting a system-specific manifest**, never by branching
//! here. `tests/profile_fixtures.rs` is the gate on that invariant: it
//! reproduces every assertion in `docs/defaults-reference.rs.txt` from a
//! synthetic non-Firestream JSON profile alone.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pipeline::Tier;

/// The only schema version this reader accepts.
pub const SCHEMA_VERSION: u32 = 1;

/// Artifact-kind strings the export resolver is allowed to emit. Kept as a
/// constant so [`Profile::validate`] can reject a typo in the profile rather
/// than silently producing `ArtifactKind::Other("imgae")` at export time.
///
/// Mirrors `crate::manifest::ArtifactKind`'s variants; `Other` is the escape
/// hatch and is deliberately NOT in this list — an unrecognised kind is a
/// profile bug, not a feature.
pub const KNOWN_ARTIFACT_KINDS: &[&str] = &[
    "image",
    "container",
    "binary",
    "wasm",
    "sbom",
    "app",
    "nix",
];

// ───────────────────────────────────────────────────────────────────────────
// Profile
// ───────────────────────────────────────────────────────────────────────────

/// A complete CI profile, parsed from `ci-manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Schema version. Must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,

    /// Project identity — name, target Nix system, target container arch.
    #[serde(default)]
    pub project: Project,

    /// Ordered phase declarations. Execution order is the topological order of
    /// `depends_on`, NOT the array order (see [`Profile::phase_order`]).
    #[serde(default)]
    pub phases: Vec<PhaseSpec>,

    /// Ordered attr-leaf → [`Tier`] classification rules. First match wins.
    #[serde(default)]
    pub tier_rules: Vec<TierRule>,

    /// Tier assigned when no rule in `tier_rules` matches. The reference
    /// implementation's comment is the reason this defaults to `Required`: an
    /// unintentional advisory typo must not silently downgrade a gate.
    #[serde(default = "default_tier")]
    pub tier_default: Tier,

    /// Ordered `build`-phase export rules. First match wins; see
    /// [`ExportTarget`] for the matching and templating vocabulary.
    #[serde(default)]
    pub export_targets: Vec<ExportTarget>,

    /// Fallback when no `export_targets` rule matches.
    #[serde(default)]
    pub export_default: ExportOutcome,

    /// Env vars forwarded from the host into the builder container.
    #[serde(default)]
    pub passthrough_vars: Vec<String>,

    /// Env sentinels that prove "we are inside the project's Nix devshell".
    #[serde(default)]
    pub devshell_sentinels: Vec<EnvSentinel>,

    /// Builder-image identity, base, size floor, container naming.
    #[serde(default)]
    pub builder: Builder,

    /// Native-vs-docker build/cache policy. Consumed by Phase 5's
    /// `platform` unification; carried in v1 so the schema does not need a
    /// version bump then.
    #[serde(default)]
    pub build_strategy: BuildStrategy,

    /// `<container>:<version>` → Nix package attribute name, with an empty
    /// version meaning "that container's default". Read by
    /// [`Profile::resolve_package_name`], which is a bug-for-bug mirror of
    /// `bin/build/_common.sh::resolve_package_name`.
    ///
    /// This has to be data, and it has to be a *table*: the flake aliases
    /// `.#redis` to redis-8 while the build path's bare `redis` is redis-7,
    /// and `odoo:` resolves to the unsuffixed `odoo`. Golden vectors live in
    /// `bin/build/registry-cases.json`, which the Nix producer reads verbatim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub container_registry: BTreeMap<String, String>,

    /// Free-form provenance emitted by the Nix producer (flake rev, etc.).
    /// Never interpreted by Rust.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance: BTreeMap<String, String>,
}

fn default_tier() -> Tier {
    Tier::Required
}

impl Default for Profile {
    /// A minimal, **project-free** profile.
    ///
    /// This is what [`crate::profile::resolve`] falls back to when no
    /// `ci-manifest.json` can be found, and it is deliberately almost empty:
    /// no phases, no attrs, no export rules, no passthrough vars. The single
    /// non-empty field is `devshell_sentinels = [IN_NIX_SHELL]` — `IN_NIX_SHELL`
    /// is *Nix's own* env var, set by `nix develop` for every project on earth,
    /// so honouring it is a property of the toolchain rather than knowledge
    /// about any particular repository. `FIRESTREAM_DEVSHELL` is Firestream's
    /// value and lives in Firestream's profile, not here.
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            project: Project::default(),
            phases: Vec::new(),
            tier_rules: Vec::new(),
            tier_default: Tier::Required,
            export_targets: Vec::new(),
            export_default: ExportOutcome::default(),
            passthrough_vars: Vec::new(),
            devshell_sentinels: vec![EnvSentinel {
                name: "IN_NIX_SHELL".to_string(),
                value: None,
            }],
            builder: Builder::default(),
            build_strategy: BuildStrategy::default(),
            container_registry: BTreeMap::new(),
            provenance: BTreeMap::new(),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// project
// ───────────────────────────────────────────────────────────────────────────

/// Project identity. `nix_system` / `arch` are the values the Nix producer
/// baked this manifest for; the CLI may override them at runtime, in which
/// case the runtime values win for `{system}` / `{arch}` expansion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// Short project slug (`firestream`). Used for `{project}` expansion and
    /// as the default stem for the derived prefixes below.
    #[serde(default = "default_project_name")]
    pub name: String,

    /// Nix system double the profile was emitted for (`x86_64-linux`).
    #[serde(default)]
    pub nix_system: String,

    /// Container/target arch (`x86_64`).
    #[serde(default)]
    pub arch: String,

    /// Prefix for `firestream-ci k8s namespace` output. The Nix producer
    /// defaults it to `"${name}-"`; carried explicitly so Rust never has to
    /// know how a project spells its namespaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k8s_namespace_prefix: Option<String>,
}

fn default_project_name() -> String {
    "unnamed".to_string()
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: default_project_name(),
            nix_system: String::new(),
            arch: String::new(),
            k8s_namespace_prefix: None,
        }
    }
}

impl Project {
    /// Namespace prefix, falling back to `"<name>-"`.
    pub fn k8s_namespace_prefix(&self) -> String {
        self.k8s_namespace_prefix
            .clone()
            .unwrap_or_else(|| format!("{}-", self.name))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// phases
// ───────────────────────────────────────────────────────────────────────────

/// One pipeline phase.
///
/// `attrs` / `advisory_attrs` are `{system}` / `{arch}` / `{project}`
/// templates (see [`Profile::expand`]). They are NOT arch-gated here: a
/// profile that must differ per system is emitted per system by Nix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSpec {
    /// Phase name (`tidy`, `verify`, `build`, `attest`).
    pub name: String,

    /// Phase-level tier. `required` phases gate the run; `advisory` phases
    /// report and downgrade the exit code to 2 at worst.
    #[serde(default = "default_tier")]
    pub tier: Tier,

    /// Names of phases that must complete before this one.
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// CI modes this phase runs in (`check`, `release`). Empty ⇒ every mode.
    #[serde(default)]
    pub modes: Vec<String>,

    /// Attribute templates whose tier comes from `tier_rules` / `tier_default`.
    #[serde(default)]
    pub attrs: Vec<String>,

    /// Attribute templates forced to [`Tier::Advisory`] regardless of
    /// `tier_rules`.
    ///
    /// Both mechanisms exist because the reference has both: ConceptDB named
    /// its checks `required-*` / `advisory-*` and classified by prefix, while
    /// Firestream's `nix/flake-modules/checks.nix` uses `firestream-*` with no
    /// tier convention in the name at all. A prefix classifier alone cannot
    /// express Firestream; an explicit list alone cannot express ConceptDB's
    /// "a new `advisory-` check is advisory the moment it is added".
    #[serde(default)]
    pub advisory_attrs: Vec<String>,

    /// Built-in runner tasks this phase runs, from the tool's own fixed
    /// vocabulary (see [`BUILTIN_TASKS`]). These are the phase steps that are
    /// *not* nix attributes — repo hygiene, store GC — so they cannot be
    /// expressed as `attrs`, but they are still declared as data rather than
    /// hardcoded per phase name in the runner.
    #[serde(default)]
    pub builtin_tasks: Vec<String>,

    /// Arbitrary commands run as parallel tasks in this phase. This is the
    /// seam that keeps a non-hermetic step (an e2e harness that binds a
    /// socket, a deploy smoke test) expressible without any project-specific
    /// Rust. Each command's argv is expanded with `{system}`/`{arch}`/
    /// `{project}` like everything else.
    #[serde(default)]
    pub shell_tasks: Vec<ShellTask>,

    /// Collapse this phase's `attrs` into ONE `nix-fast-build` invocation over
    /// their common attribute-path prefix, instead of one invocation per attr.
    ///
    /// This is the difference between 22 concurrent multi-GB flake evaluations
    /// fighting over the store lock and a single evaluation feeding one bounded
    /// job queue with per-attribute failure isolation. Per-attr verdicts are
    /// unchanged: the result file still carries one entry per attribute.
    ///
    /// Requires the phase's attrs to share a dotted prefix and
    /// `advisory_attrs` to be empty (an aggregate task has exactly one tier —
    /// the phase's). The runner falls back to per-attr invocations, with a
    /// warning, when either does not hold.
    #[serde(default)]
    pub aggregate: bool,

    /// Materialise this phase's build outputs into `<rundir>/artifacts/` (with
    /// sha256 + size) via the top-level `export_targets` rules, and record a
    /// manifest entry for each. Off by default: a `verify` phase's check
    /// derivations are not release artifacts.
    #[serde(default)]
    pub export_artifacts: bool,
}

/// The complete built-in task vocabulary. A phase naming anything else is a
/// profile error surfaced at load time, not a silent no-op.
pub const BUILTIN_TASKS: &[&str] = &["nix-gc", "target-sweep"];

/// One free-form command task in a phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellTask {
    /// Task name as it appears in the dashboard, summary and log file names.
    pub name: String,
    /// Full argv. `command[0]` is the program; nothing is shell-interpreted.
    pub command: Vec<String>,
}

impl PhaseSpec {
    /// Whether this phase runs in `mode`. An empty `modes` list means "always".
    pub fn runs_in_mode(&self, mode: &str) -> bool {
        self.modes.is_empty() || self.modes.iter().any(|m| m == mode)
    }

    /// True when the phase would materialise at least one task.
    pub fn has_work(&self) -> bool {
        !self.attrs.is_empty()
            || !self.advisory_attrs.is_empty()
            || !self.builtin_tasks.is_empty()
            || !self.shell_tasks.is_empty()
    }
}

/// Longest common `.`-separated attribute-path prefix of `attrs`, or `None`
/// when the list is empty, has fewer than two segments, or does not share one.
///
/// Pure string algebra over Nix attribute paths — no project knowledge.
pub fn common_attr_prefix(attrs: &[String]) -> Option<String> {
    let first = attrs.first()?;
    let mut prefix: Vec<&str> = first.split('.').collect();
    // The last segment is the leaf being built; a prefix must exclude it.
    prefix.pop();
    if prefix.is_empty() {
        return None;
    }
    for a in attrs.iter().skip(1) {
        let mut segs: Vec<&str> = a.split('.').collect();
        segs.pop();
        if segs != prefix {
            return None;
        }
    }
    Some(prefix.join("."))
}

// ───────────────────────────────────────────────────────────────────────────
// matching
// ───────────────────────────────────────────────────────────────────────────

/// A declarative string matcher. Every populated clause must hold (logical
/// AND); an entirely empty matcher matches everything, which is how a
/// catch-all terminal rule is written.
///
/// This is the whole matching vocabulary, and it is exactly what
/// `build_export_target` in `docs/defaults-reference.rs.txt` needed:
/// `starts_with` (prefix), `ends_with` (suffix), `contains` (infix), and `==`
/// (equals). No globs, no regex — the reference used none, and adding them
/// would put a matching engine's semantics into the contract for no gain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Matcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

impl Matcher {
    /// True when every populated clause holds for `s`.
    pub fn matches(&self, s: &str) -> bool {
        if let Some(e) = &self.equals {
            if s != e {
                return false;
            }
        }
        if let Some(p) = &self.prefix {
            if !s.starts_with(p.as_str()) {
                return false;
            }
        }
        if let Some(x) = &self.suffix {
            if !s.ends_with(x.as_str()) {
                return false;
            }
        }
        if let Some(c) = &self.contains {
            if !s.contains(c.as_str()) {
                return false;
            }
        }
        true
    }

    /// True when no clause is populated (i.e. this is a catch-all).
    pub fn is_catch_all(&self) -> bool {
        self == &Matcher::default()
    }
}

/// One attr-leaf → tier classification rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierRule {
    #[serde(rename = "match", default)]
    pub matcher: Matcher,
    pub tier: Tier,
}

// ───────────────────────────────────────────────────────────────────────────
// export targets
// ───────────────────────────────────────────────────────────────────────────

/// The resolved outcome of export-target resolution: where the artifact lands
/// inside `<rundir>/artifacts/`, and how it is classified in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOutcome {
    /// Destination sub-path template. See [`ExportTarget`] for the vocabulary.
    pub dest: String,
    /// Artifact kind string; must be one of [`KNOWN_ARTIFACT_KINDS`].
    pub kind: String,
}

impl Default for ExportOutcome {
    /// Matches the reference's `else` arm: identity dest, `binary` kind.
    fn default() -> Self {
        Self {
            dest: "{leaf}".to_string(),
            kind: "binary".to_string(),
        }
    }
}

/// One `build`-phase export rule.
///
/// ## Why this shape
///
/// `build_export_target` in `docs/defaults-reference.rs.txt` is the hardest
/// thing in the inventory to express declaratively — it does prefix matching,
/// suffix matching, an infix `contains`, and a strip-prefix/strip-suffix
/// transform feeding a nested destination path. The shape below covers all of
/// it with three moving parts and no escape hatch into Rust:
///
/// 1. **`match`** — a [`Matcher`] over the attr *leaf*.
/// 2. **`strip_prefix` / `strip_suffix`** — produce `{stem}` from the leaf.
///    Each is applied only if it is actually present on the leaf (a
///    non-matching strip is a no-op, mirroring the reference's
///    `.unwrap_or(attr)`).
/// 3. **`dest` / `kind`** — a destination template and a kind string.
///
/// ## Template vocabulary
///
/// `dest` expands `{leaf}`, `{stem}`, `{arch}`, `{system}`, `{project}`.
///
/// The plan constrains templating to `{system}` / `{arch}`; `{leaf}` and
/// `{stem}` are a deliberate, narrow extension confined to `dest`. They are
/// not project knowledge — they are the rule's own captures, the declarative
/// equivalent of a regex backreference. Without them the wasm case
/// (`conceptdb-portal-wasm` → `conceptdb-wasm/portal`) would have to be
/// enumerated one rule per artifact, which pushes the project's artifact
/// inventory into two places instead of one. The invariant the constraint
/// protects — *no project reasoning in Rust* — is untouched: the resolver
/// still only matches, strips, and substitutes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTarget {
    #[serde(rename = "match", default)]
    pub matcher: Matcher,

    /// Removed from the front of the leaf when computing `{stem}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_prefix: Option<String>,

    /// Removed from the end of the leaf when computing `{stem}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_suffix: Option<String>,

    /// Destination sub-path template under `<rundir>/artifacts/`.
    pub dest: String,

    /// Artifact kind string.
    pub kind: String,
}

impl ExportTarget {
    /// `{stem}` for `leaf`: the leaf with `strip_prefix` / `strip_suffix`
    /// removed when present.
    pub fn stem(&self, leaf: &str) -> String {
        let mut s = leaf;
        if let Some(p) = &self.strip_prefix {
            s = s.strip_prefix(p.as_str()).unwrap_or(s);
        }
        if let Some(x) = &self.strip_suffix {
            s = s.strip_suffix(x.as_str()).unwrap_or(s);
        }
        s.to_string()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// env sentinels / builder / strategy
// ───────────────────────────────────────────────────────────────────────────

/// An env-var sentinel. `value == None` means "present with any value";
/// `Some(v)` means "present and exactly equal to `v`".
///
/// Both forms are needed by the reference behaviour this replaces: the devshell
/// guard tested `IN_NIX_SHELL` for *presence* (Nix sets it to `pure` or
/// `impure`) but `<PROJECT>_DEVSHELL` for the exact string `1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSentinel {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl EnvSentinel {
    /// Evaluate this sentinel against a `getenv`-shaped callable.
    pub fn is_set<F>(&self, getenv: &F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        match (getenv(&self.name), self.value.as_deref()) {
            (Some(_), None) => true,
            (Some(actual), Some(want)) => actual == want,
            (None, _) => false,
        }
    }
}

/// Builder-image identity and guards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Builder {
    /// Bare builder image name (`firestream-builder`).
    #[serde(default)]
    pub image_name: String,

    /// Base image for cold-start builds (`nixos/nix:latest`).
    #[serde(default)]
    pub base_image: String,

    /// Minimum legitimate builder-image size in bytes. A flattened image
    /// smaller than this is rejected as corrupt/empty.
    #[serde(default = "default_min_image_size")]
    pub min_image_size_bytes: u64,

    /// Prefix used to name (and, in the reaper, to match) transient CI
    /// containers. The Nix producer defaults it to `"${project.name}-ci-"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name_prefix: Option<String>,
}

/// 100 MiB — the reference's `MIN_BUILDER_IMAGE_SIZE_BYTES`.
fn default_min_image_size() -> u64 {
    100 * 1024 * 1024
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            image_name: String::new(),
            base_image: String::new(),
            min_image_size_bytes: default_min_image_size(),
            container_name_prefix: None,
        }
    }
}

/// Native-vs-docker build and cache policy (Part C of the plan).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStrategy {
    /// `auto` | `native` | `docker`.
    #[serde(default = "default_strategy")]
    pub default: String,

    /// Cache backend used by the native strategy (`host-store`).
    #[serde(default)]
    pub native_cache: String,

    /// Docker cache volume name template; `{arch}` is expanded.
    #[serde(default)]
    pub docker_cache_volume: String,
}

fn default_strategy() -> String {
    "auto".to_string()
}

impl Default for BuildStrategy {
    fn default() -> Self {
        Self {
            default: default_strategy(),
            native_cache: String::new(),
            docker_cache_volume: String::new(),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Resolution API
// ───────────────────────────────────────────────────────────────────────────

/// The runtime values `{system}` / `{arch}` expand to. Defaults come from
/// `project`, but the CLI's `--nix-system` / `--arch` flags override them, so
/// expansion takes an explicit context rather than reading `self.project`.
#[derive(Debug, Clone)]
pub struct ExpandCtx {
    pub system: String,
    pub arch: String,
    pub project: String,
}

impl Profile {
    /// Expansion context from the profile's own `project` block.
    pub fn expand_ctx(&self) -> ExpandCtx {
        ExpandCtx {
            system: self.project.nix_system.clone(),
            arch: self.project.arch.clone(),
            project: self.project.name.clone(),
        }
    }

    /// Expand `{system}` / `{arch}` / `{project}` in `template`.
    ///
    /// Unknown placeholders are left verbatim — a profile that writes `{oops}`
    /// gets a loudly wrong attr name rather than a silently empty one.
    pub fn expand(&self, template: &str, ctx: &ExpandCtx) -> String {
        template
            .replace("{system}", &ctx.system)
            .replace("{arch}", &ctx.arch)
            .replace("{project}", &ctx.project)
    }

    /// Look up a phase by name.
    pub fn phase(&self, name: &str) -> Option<&PhaseSpec> {
        self.phases.iter().find(|p| p.name == name)
    }

    /// Expanded required-attr list for `name` (empty when the phase is absent).
    pub fn phase_attrs(&self, name: &str, ctx: &ExpandCtx) -> Vec<String> {
        self.phase(name)
            .map(|p| p.attrs.iter().map(|a| self.expand(a, ctx)).collect())
            .unwrap_or_default()
    }

    /// Expanded advisory-attr list for `name`.
    pub fn phase_advisory_attrs(&self, name: &str, ctx: &ExpandCtx) -> Vec<String> {
        self.phase(name)
            .map(|p| {
                p.advisory_attrs
                    .iter()
                    .map(|a| self.expand(a, ctx))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `(prefix_attr, leaf_names)` an aggregated phase collapses to, or
    /// `Err(reason)` when aggregation is not possible and the caller must fall
    /// back to per-attr invocations.
    ///
    /// Returns `Ok(None)` when the phase does not opt into aggregation.
    pub fn phase_aggregate(
        &self,
        name: &str,
        ctx: &ExpandCtx,
    ) -> Result<Option<(String, Vec<String>)>, String> {
        let Some(p) = self.phase(name) else {
            return Ok(None);
        };
        if !p.aggregate {
            return Ok(None);
        }
        if !p.advisory_attrs.is_empty() {
            return Err(format!(
                "phase `{name}` sets aggregate=true but declares advisory_attrs; \
                 an aggregate task carries exactly one tier"
            ));
        }
        let attrs = self.phase_attrs(name, ctx);
        if attrs.len() < 2 {
            return Ok(None);
        }
        let Some(prefix) = common_attr_prefix(&attrs) else {
            return Err(format!(
                "phase `{name}` sets aggregate=true but its attrs do not share \
                 a common attribute-path prefix"
            ));
        };
        let leaves: Vec<String> = attrs.iter().map(|a| attr_leaf(a).to_string()).collect();
        Ok(Some((prefix, leaves)))
    }

    /// Phases that run in `mode` **and** would materialise at least one task.
    ///
    /// The distinction matters: a profile emitted for a system where every
    /// attr list is empty (Firestream's Darwin manifest — checks and container
    /// images are Linux-only) still declares four phases, and running it would
    /// otherwise be a silent green no-op.
    pub fn runnable_phases(&self, mode: &str) -> Vec<&PhaseSpec> {
        self.phases
            .iter()
            .filter(|p| p.runs_in_mode(mode) && p.has_work())
            .collect()
    }

    /// Classify an attribute into a [`Tier`].
    ///
    /// Accepts either a full attr path (`checks.x86_64-linux.required-rust-fmt`)
    /// or a bare leaf (`required-rust-fmt`): the last `.`-separated segment is
    /// what rules are tested against. Splitting on `.` is a property of Nix
    /// attribute paths, not of any project.
    ///
    /// Attrs listed in a phase's `advisory_attrs` are forced advisory by
    /// [`Profile::tier_of_in_phase`]; this function sees only `tier_rules`.
    pub fn tier_of(&self, attr: &str) -> Tier {
        let leaf = attr_leaf(attr);
        for rule in &self.tier_rules {
            if rule.matcher.matches(leaf) {
                return rule.tier;
            }
        }
        self.tier_default
    }

    /// [`Profile::tier_of`], narrowed by phase context.
    ///
    /// Two things can only ever *downgrade* an attr to [`Tier::Advisory`]:
    ///
    /// 1. The phase itself is advisory. A required attr inside an advisory
    ///    phase is a contradiction — the phase's failure cannot fail the run,
    ///    so neither can the attr's. Reporting it as `required` would put a
    ///    misleading label on its log file and in the run summary.
    /// 2. The attr is listed in the phase's `advisory_attrs`.
    ///
    /// Nothing here can *upgrade* an attr to required, which keeps the
    /// conservative direction of `tier_default` intact.
    pub fn tier_of_in_phase(&self, phase: &str, attr: &str, ctx: &ExpandCtx) -> Tier {
        if let Some(p) = self.phase(phase) {
            if p.tier == Tier::Advisory {
                return Tier::Advisory;
            }
        }
        if self
            .phase_advisory_attrs(phase, ctx)
            .iter()
            .any(|a| a == attr || attr_leaf(a) == attr_leaf(attr))
        {
            return Tier::Advisory;
        }
        self.tier_of(attr)
    }

    /// Resolve a `build`-phase attr leaf to `(dest_subdir, kind)`.
    ///
    /// Rules are evaluated in order; the first whose [`Matcher`] accepts the
    /// leaf wins. Falls through to `export_default`.
    pub fn resolve_export(&self, leaf: &str, ctx: &ExpandCtx) -> (String, String) {
        for rule in &self.export_targets {
            if rule.matcher.matches(leaf) {
                let stem = rule.stem(leaf);
                let dest = self
                    .expand(&rule.dest, ctx)
                    .replace("{leaf}", leaf)
                    .replace("{stem}", &stem);
                return (dest, rule.kind.clone());
            }
        }
        let dest = self
            .expand(&self.export_default.dest, ctx)
            .replace("{leaf}", leaf)
            .replace("{stem}", leaf);
        (dest, self.export_default.kind.clone())
    }

    /// Builder container-name prefix, defaulting to `"<project>-ci-"`.
    pub fn container_name_prefix(&self) -> String {
        self.builder
            .container_name_prefix
            .clone()
            .unwrap_or_else(|| format!("{}-ci-", self.project.name))
    }

    /// True when any configured devshell sentinel is satisfied. An empty
    /// sentinel list yields `false` (no sentinel ⇒ no proof).
    pub fn in_devshell<F>(&self, getenv: &F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        self.devshell_sentinels.iter().any(|s| s.is_set(getenv))
    }

    /// Human-readable rendering of the sentinel set, for the guard's error
    /// message ("must run inside the Nix devshell").
    pub fn devshell_sentinel_summary(&self) -> String {
        if self.devshell_sentinels.is_empty() {
            return "(none configured)".to_string();
        }
        self.devshell_sentinels
            .iter()
            .map(|s| match &s.value {
                Some(v) => format!("{}={}", s.name, v),
                None => format!("{} (any value)", s.name),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Phases in dependency order, restricted to those that run in `mode`.
    ///
    /// Kahn's algorithm over `depends_on`, with ties broken by declaration
    /// order so the output is deterministic. Dependencies that are filtered
    /// out by `mode` are treated as satisfied — a `build` phase depending on a
    /// `verify` phase that does not run in this mode still runs.
    pub fn phase_order(&self, mode: &str) -> Result<Vec<&PhaseSpec>, ProfileError> {
        let active: Vec<&PhaseSpec> = self
            .phases
            .iter()
            .filter(|p| p.runs_in_mode(mode))
            .collect();
        let active_names: Vec<&str> = active.iter().map(|p| p.name.as_str()).collect();

        let mut remaining: Vec<&PhaseSpec> = active.clone();
        let mut done: Vec<&str> = Vec::new();
        let mut out: Vec<&PhaseSpec> = Vec::new();

        while !remaining.is_empty() {
            let idx = remaining.iter().position(|p| {
                p.depends_on.iter().all(|d| {
                    // Satisfied if already emitted, or not active in this mode.
                    done.contains(&d.as_str()) || !active_names.contains(&d.as_str())
                })
            });
            match idx {
                Some(i) => {
                    let p = remaining.remove(i);
                    done.push(p.name.as_str());
                    out.push(p);
                }
                None => {
                    let stuck: Vec<String> =
                        remaining.iter().map(|p| p.name.clone()).collect();
                    return Err(ProfileError::PhaseCycle(stuck.join(", ")));
                }
            }
        }
        Ok(out)
    }

    /// Structural validation. Run once at load time so a malformed profile
    /// fails at the CLI boundary rather than midway through a pipeline.
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ProfileError::SchemaVersion {
                found: self.schema_version,
                expected: SCHEMA_VERSION,
            });
        }

        // Phase names unique.
        let mut seen: Vec<&str> = Vec::new();
        for p in &self.phases {
            if seen.contains(&p.name.as_str()) {
                return Err(ProfileError::DuplicatePhase(p.name.clone()));
            }
            seen.push(&p.name);
        }

        // depends_on references resolve.
        for p in &self.phases {
            for d in &p.depends_on {
                if !seen.contains(&d.as_str()) {
                    return Err(ProfileError::UnknownDependency {
                        phase: p.name.clone(),
                        dependency: d.clone(),
                    });
                }
            }
        }

        // No cycles (checked against the unfiltered phase set).
        self.phase_order("")?;

        // Built-in task names come from a closed vocabulary. A typo here would
        // otherwise be a phase that silently does nothing.
        for p in &self.phases {
            for t in &p.builtin_tasks {
                if !BUILTIN_TASKS.contains(&t.as_str()) {
                    return Err(ProfileError::UnknownBuiltinTask {
                        phase: p.name.clone(),
                        task: t.clone(),
                    });
                }
            }
            for s in &p.shell_tasks {
                if s.command.is_empty() {
                    return Err(ProfileError::EmptyShellCommand {
                        phase: p.name.clone(),
                        task: s.name.clone(),
                    });
                }
            }
        }

        // Export kinds are recognised.
        for r in &self.export_targets {
            if !KNOWN_ARTIFACT_KINDS.contains(&r.kind.as_str()) {
                return Err(ProfileError::UnknownArtifactKind(r.kind.clone()));
            }
        }
        if !KNOWN_ARTIFACT_KINDS.contains(&self.export_default.kind.as_str()) {
            return Err(ProfileError::UnknownArtifactKind(
                self.export_default.kind.clone(),
            ));
        }

        Ok(())
    }
}

impl Profile {
    /// `<container>` + `<version>` → Nix package attribute name.
    ///
    /// Bug-for-bug mirror of `bin/build/_common.sh::resolve_package_name`:
    ///
    /// 1. exact key `<container>:<version>`;
    /// 2. else the family default key `<container>:` — note this means an
    ///    *unrecognised version* silently falls back to the default rather
    ///    than erroring, which is the shell's behaviour;
    /// 3. else fail.
    ///
    /// The empty registry is an error, not an empty lookup: a profile that
    /// forgot the table would otherwise make every container "unknown" and the
    /// message would blame the container instead of the profile.
    pub fn resolve_package_name(
        &self,
        container: &str,
        version: &str,
    ) -> Result<String, ProfileError> {
        if self.container_registry.is_empty() {
            return Err(ProfileError::EmptyContainerRegistry);
        }
        let exact = format!("{container}:{version}");
        if let Some(pkg) = self.container_registry.get(&exact) {
            return Ok(pkg.clone());
        }
        let default_key = format!("{container}:");
        if let Some(pkg) = self.container_registry.get(&default_key) {
            return Ok(pkg.clone());
        }
        Err(ProfileError::UnknownContainer {
            container: container.to_string(),
            version: if version.is_empty() {
                "default".to_string()
            } else {
                version.to_string()
            },
        })
    }

    /// Bare container names the registry knows, in sorted order. Purely for
    /// the CLI's "available containers" hint.
    pub fn known_containers(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .container_registry
            .keys()
            .filter_map(|k| k.split_once(':').map(|(c, _)| c.to_string()))
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// Last `.`-separated segment of a Nix attribute path.
pub fn attr_leaf(attr: &str) -> &str {
    attr.rsplit('.').next().unwrap_or(attr)
}

// ───────────────────────────────────────────────────────────────────────────
// Errors
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("profile: not found (looked at: {0})")]
    NotFound(String),

    #[error(
        "profile: phase `{phase}` names unknown builtin task `{task}` \
         (known: nix-gc, target-sweep)"
    )]
    UnknownBuiltinTask { phase: String, task: String },

    #[error("profile: phase `{phase}` shell task `{task}` has an empty command")]
    EmptyShellCommand { phase: String, task: String },

    #[error("profile: reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("profile: parsing {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("profile: schema_version {found} is not supported (expected {expected})")]
    SchemaVersion { found: u32, expected: u32 },

    #[error("profile: duplicate phase `{0}`")]
    DuplicatePhase(String),

    #[error("profile: phase `{phase}` depends on unknown phase `{dependency}`")]
    UnknownDependency { phase: String, dependency: String },

    #[error("profile: dependency cycle among phases: {0}")]
    PhaseCycle(String),

    #[error("profile: unknown artifact kind `{0}` (expected one of: image, container, binary, wasm, sbom, app, nix)")]
    UnknownArtifactKind(String),

    #[error(
        "profile: `container_registry` is empty — this profile cannot map a \
         container name to a Nix package. Rebuild the CI profile \
         (nix build .#firestream-ci-profile) or point FIRESTREAM_CI_PROFILE at one."
    )]
    EmptyContainerRegistry,

    #[error("profile: unknown container `{container}` (version: {version})")]
    UnknownContainer { container: String, version: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_and_semantics() {
        let m = Matcher {
            prefix: Some("acme-".into()),
            contains: Some("-linux-".into()),
            ..Default::default()
        };
        assert!(m.matches("acme-server-linux-x86_64"));
        assert!(!m.matches("acme-sbom"));
        assert!(!m.matches("other-server-linux-x86_64"));
    }

    #[test]
    fn empty_matcher_is_catch_all() {
        let m = Matcher::default();
        assert!(m.is_catch_all());
        assert!(m.matches("anything at all"));
    }

    #[test]
    fn attr_leaf_strips_nix_path() {
        assert_eq!(attr_leaf("checks.x86_64-linux.required-rust-fmt"), "required-rust-fmt");
        assert_eq!(attr_leaf("required-rust-fmt"), "required-rust-fmt");
    }

    #[test]
    fn env_sentinel_presence_vs_exact() {
        let presence = EnvSentinel { name: "IN_NIX_SHELL".into(), value: None };
        let exact = EnvSentinel { name: "X_DEVSHELL".into(), value: Some("1".into()) };
        let env = |k: &str| match k {
            "IN_NIX_SHELL" => Some("impure".to_string()),
            "X_DEVSHELL" => Some("0".to_string()),
            _ => None,
        };
        assert!(presence.is_set(&env));
        assert!(!exact.is_set(&env));
    }

    #[test]
    fn default_profile_is_project_free() {
        let p = Profile::default();
        assert!(p.phases.is_empty());
        assert!(p.export_targets.is_empty());
        assert!(p.passthrough_vars.is_empty());
        assert!(p.tier_rules.is_empty());
        // The one non-empty field: Nix's own env var, not any project's.
        assert_eq!(p.devshell_sentinels.len(), 1);
        assert_eq!(p.devshell_sentinels[0].name, "IN_NIX_SHELL");
        p.validate().expect("default profile must validate");
    }

    #[test]
    fn phase_order_is_topological() {
        let json = serde_json::json!({
            "schema_version": 1,
            "phases": [
                { "name": "attest", "depends_on": ["build"] },
                { "name": "build",  "depends_on": ["verify"] },
                { "name": "tidy",   "depends_on": [] },
                { "name": "verify", "depends_on": ["tidy"] }
            ]
        });
        let p: Profile = serde_json::from_value(json).unwrap();
        p.validate().unwrap();
        let order: Vec<&str> = p.phase_order("release").unwrap().iter().map(|x| x.name.as_str()).collect();
        assert_eq!(order, vec!["tidy", "verify", "build", "attest"]);
    }

    #[test]
    fn phase_order_detects_cycle() {
        let json = serde_json::json!({
            "schema_version": 1,
            "phases": [
                { "name": "a", "depends_on": ["b"] },
                { "name": "b", "depends_on": ["a"] }
            ]
        });
        let p: Profile = serde_json::from_value(json).unwrap();
        assert!(matches!(p.validate(), Err(ProfileError::PhaseCycle(_))));
    }

    #[test]
    fn mode_filter_drops_release_only_phases() {
        let json = serde_json::json!({
            "schema_version": 1,
            "phases": [
                { "name": "verify", "depends_on": [] },
                { "name": "build",  "depends_on": ["verify"], "modes": ["release"] },
                { "name": "attest", "depends_on": ["build"] }
            ]
        });
        let p: Profile = serde_json::from_value(json).unwrap();
        p.validate().unwrap();
        let order: Vec<&str> = p.phase_order("check").unwrap().iter().map(|x| x.name.as_str()).collect();
        // `attest` still runs: its dependency was filtered out, not failed.
        assert_eq!(order, vec!["verify", "attest"]);
    }

    #[test]
    fn schema_version_mismatch_is_rejected() {
        let json = serde_json::json!({ "schema_version": 99 });
        let p: Profile = serde_json::from_value(json).unwrap();
        assert!(matches!(p.validate(), Err(ProfileError::SchemaVersion { .. })));
    }

    #[test]
    fn unknown_artifact_kind_is_rejected() {
        let json = serde_json::json!({
            "schema_version": 1,
            "export_targets": [ { "match": {}, "dest": "{leaf}", "kind": "imgae" } ]
        });
        let p: Profile = serde_json::from_value(json).unwrap();
        assert!(matches!(p.validate(), Err(ProfileError::UnknownArtifactKind(_))));
    }
}

/// Phase-6 vocabulary: builtin/shell tasks, aggregation, export opt-in, and
/// the runnable-phase invariant that keeps a misconfigured CI from exiting
/// green.
#[cfg(test)]
mod phase_vocabulary_tests {
    use super::*;

    fn profile(phases: serde_json::Value) -> Profile {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "project": { "name": "acme", "nix_system": "x86_64-linux", "arch": "x86_64" },
            "phases": phases,
        }))
        .unwrap()
    }

    #[test]
    fn common_prefix_of_a_uniform_attr_list() {
        let attrs = vec![
            "packages.x86_64-linux.airflow".to_string(),
            "packages.x86_64-linux.airflow-chart".to_string(),
            "packages.x86_64-linux.firestream-charts-bundle".to_string(),
        ];
        assert_eq!(
            common_attr_prefix(&attrs).as_deref(),
            Some("packages.x86_64-linux")
        );
    }

    #[test]
    fn common_prefix_rejects_a_mixed_list() {
        let attrs = vec![
            "packages.x86_64-linux.airflow".to_string(),
            "checks.x86_64-linux.fmt".to_string(),
        ];
        assert_eq!(common_attr_prefix(&attrs), None);
        // A single-segment attr has no prefix to aggregate over.
        assert_eq!(common_attr_prefix(&["hello".to_string()]), None);
        assert_eq!(common_attr_prefix(&[]), None);
    }

    #[test]
    fn aggregate_collapses_a_uniform_phase() {
        let p = profile(serde_json::json!([{
            "name": "build",
            "tier": "required",
            "aggregate": true,
            "attrs": ["packages.{system}.a", "packages.{system}.b"],
        }]));
        let (prefix, leaves) = p
            .phase_aggregate("build", &p.expand_ctx())
            .unwrap()
            .expect("aggregation applies");
        assert_eq!(prefix, "packages.x86_64-linux");
        assert_eq!(leaves, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn aggregate_refuses_when_the_phase_has_advisory_attrs() {
        // An aggregate task carries exactly one tier; per-attr tiers cannot
        // survive the collapse, so this must degrade rather than lie.
        let p = profile(serde_json::json!([{
            "name": "build",
            "aggregate": true,
            "attrs": ["packages.{system}.a", "packages.{system}.b"],
            "advisory_attrs": ["packages.{system}.flaky"],
        }]));
        assert!(p.phase_aggregate("build", &p.expand_ctx()).is_err());
    }

    #[test]
    fn aggregate_refuses_a_non_uniform_phase() {
        let p = profile(serde_json::json!([{
            "name": "mixed",
            "aggregate": true,
            "attrs": ["packages.{system}.a", "checks.{system}.b"],
        }]));
        assert!(p.phase_aggregate("mixed", &p.expand_ctx()).is_err());
    }

    #[test]
    fn opting_out_of_aggregation_is_the_default() {
        let p = profile(serde_json::json!([{
            "name": "build",
            "attrs": ["packages.{system}.a", "packages.{system}.b"],
        }]));
        assert!(p.phase_aggregate("build", &p.expand_ctx()).unwrap().is_none());
    }

    /// The Darwin case: four declared phases, every one empty. `phase_order`
    /// happily returns all four; `runnable_phases` returns none, which is what
    /// makes `ci-linux` hard-error instead of exiting green.
    #[test]
    fn an_all_empty_profile_has_no_runnable_phases() {
        let p = profile(serde_json::json!([
            { "name": "tidy", "tier": "advisory" },
            { "name": "verify", "depends_on": ["tidy"] },
            { "name": "build", "depends_on": ["verify"], "modes": ["release"] },
            { "name": "attest", "tier": "advisory", "depends_on": ["build"] },
        ]));
        assert_eq!(p.phase_order("release").unwrap().len(), 4);
        assert!(p.runnable_phases("release").is_empty());
        assert!(p.runnable_phases("check").is_empty());
    }

    #[test]
    fn builtin_tasks_alone_make_a_phase_runnable() {
        let p = profile(serde_json::json!([
            { "name": "tidy", "tier": "advisory", "builtin_tasks": ["nix-gc", "target-sweep"] },
            { "name": "verify" },
        ]));
        let names: Vec<&str> = p
            .runnable_phases("release")
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["tidy"]);
    }

    #[test]
    fn shell_tasks_alone_make_a_phase_runnable() {
        let p = profile(serde_json::json!([{
            "name": "e2e",
            "tier": "advisory",
            "shell_tasks": [{ "name": "e2e-k8s", "command": ["make", "test-e2e-k8s"] }],
        }]));
        assert_eq!(p.runnable_phases("release").len(), 1);
        assert_eq!(p.phases[0].shell_tasks[0].command, vec!["make", "test-e2e-k8s"]);
    }

    #[test]
    fn mode_filtering_can_empty_the_runnable_set() {
        let p = profile(serde_json::json!([{
            "name": "build",
            "modes": ["release"],
            "attrs": ["packages.{system}.a"],
        }]));
        assert_eq!(p.runnable_phases("release").len(), 1);
        assert!(p.runnable_phases("check").is_empty());
    }

    #[test]
    fn unknown_builtin_task_is_a_load_time_error() {
        let json = serde_json::json!({
            "schema_version": 1,
            "phases": [{ "name": "tidy", "builtin_tasks": ["rm-rf-slash"] }],
        });
        let p: Profile = serde_json::from_value(json).unwrap();
        assert!(matches!(
            p.validate(),
            Err(ProfileError::UnknownBuiltinTask { .. })
        ));
    }

    #[test]
    fn empty_shell_command_is_a_load_time_error() {
        let json = serde_json::json!({
            "schema_version": 1,
            "phases": [{ "name": "e2e", "shell_tasks": [{ "name": "x", "command": [] }] }],
        });
        let p: Profile = serde_json::from_value(json).unwrap();
        assert!(matches!(
            p.validate(),
            Err(ProfileError::EmptyShellCommand { .. })
        ));
    }

    /// The new fields are all `#[serde(default)]`, so a schema-v1 document
    /// written before Phase 6 still loads unchanged.
    #[test]
    fn pre_phase6_documents_still_load() {
        let p = profile(serde_json::json!([{
            "name": "build",
            "tier": "required",
            "attrs": ["packages.{system}.a"],
        }]));
        p.validate().unwrap();
        let ph = &p.phases[0];
        assert!(ph.builtin_tasks.is_empty());
        assert!(ph.shell_tasks.is_empty());
        assert!(!ph.aggregate);
        assert!(!ph.export_artifacts);
    }
}
