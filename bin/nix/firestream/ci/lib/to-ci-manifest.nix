# ci-manifest.json emitter (schema v1)
# Copyright Firestream. MIT License.
#
# The CI-profile mirror of ./../../charts/lib/to-chart-manifest.nix. Serialises
# the evaluated option tree into the JSON document that
# `firestream_ci::profile::spec` parses.
#
# Signature: { pkgs, lib }: cfg: <derivation producing ci-manifest.json>
#
# `cfg` is the resolved `config.ci` attrset from eval-ci.nix's `lib.evalModules`.
#
# ── Two deliberate divergences from the chart emitter ────────────────────────
#
# 1. KEY CASE. The chart manifest emits camelCase because that is what the Nix
#    option names already are. This document emits SNAKE_CASE: the plan's schema
#    sketch and the `jq -e '.schema_version == 1'` acceptance check both spell it
#    that way, and it is also what serde produces for a plain Rust struct with no
#    `rename_all`. So the mapping below is explicit, field by field — the same
#    explicit style to-chart-manifest.nix uses, just remapping case as it goes.
#    The Nix side keeps house-style camelCase option names throughout.
#
# 2. VERSION TYPE. `schemaVersion = "1"` (a string) in the chart manifest;
#    `schema_version = 1` (an integer) here. Same reason: the acceptance check
#    is `== 1`.
#
# ── Ordering ────────────────────────────────────────────────────────────────
# `phases` and `devshellSentinels` are declared as ATTRSETS so the module system
# can merge contributions by name. Attrsets are unordered, and both become JSON
# ARRAYS, so each element carries an `order` int and is sorted by (order, name)
# before emission. `tierRules` and `exportTargets` are already lists and their
# order is semantic (first match wins) — they are emitted verbatim, never sorted.
#
# ── Null handling ───────────────────────────────────────────────────────────
# Same idiom as the chart emitter: `null` means "unset", and null leaves are
# stripped so the JSON stays a faithful contract. The Rust reader's documented
# fallbacks then apply (`container_name_prefix: None` -> `"<project>-ci-"`, an
# unset matcher clause -> no constraint). Explicit `""`, `[]`, `{}` are RETAINED
# as meaningful: `passthrough_vars = []` says "forward nothing", which is
# different from "the key was never declared".
{ pkgs, lib }:

cfg:

let
  # Sort an attrset-of-submodules into a list by (order, name). `order`
  # exists purely to make emission deterministic; it carries no runtime meaning.
  orderedList = attrs:
    lib.sort
      (a: b: if a.order != b.order then a.order < b.order else a.name < b.name)
      (lib.attrValues attrs);

  phase = p: {
    name = p.name;
    tier = p.tier;
    depends_on = p.dependsOn;
    modes = p.modes;
    attrs = p.attrs;
    advisory_attrs = p.advisoryAttrs;
    builtin_tasks = p.builtinTasks;
    shell_tasks = map (t: { name = t.name; command = t.command; }) p.shellTasks;
    aggregate = p.aggregate;
    export_artifacts = p.exportArtifacts;
  };

  # Matcher clauses stay camel-free already (equals/prefix/suffix/contains);
  # nulls are stripped by the recursive filter below.
  matcher = m: {
    inherit (m) equals prefix suffix contains;
  };

  tierRule = r: {
    match = matcher r.match;
    tier = r.tier;
  };

  exportTarget = t: {
    match = matcher t.match;
    strip_prefix = t.stripPrefix;
    strip_suffix = t.stripSuffix;
    dest = t.dest;
    kind = t.kind;
  };

  sentinel = s: {
    name = s.name;
    value = s.value;
  };

  manifest = {
    schema_version = 1;

    project = {
      name = cfg.project.name;
      nix_system = cfg.project.nixSystem;
      arch = cfg.project.arch;
      k8s_namespace_prefix = cfg.project.k8sNamespacePrefix;
    };

    phases = map phase (orderedList cfg.phases);

    tier_rules = map tierRule cfg.tierRules;
    tier_default = cfg.tierDefault;

    export_targets = map exportTarget cfg.exportTargets;
    export_default = {
      dest = cfg.exportDefault.dest;
      kind = cfg.exportDefault.kind;
    };

    passthrough_vars = cfg.passthroughVars;

    devshell_sentinels = map sentinel (orderedList cfg.devshellSentinels);

    builder = {
      image_name = cfg.builder.imageName;
      base_image = cfg.builder.baseImage;
      min_image_size_bytes = cfg.builder.minImageSizeBytes;
      container_name_prefix = cfg.builder.containerNamePrefix;
    };

    build_strategy = {
      default = cfg.buildStrategy.default;
      native_cache = cfg.buildStrategy.nativeCache;
      docker_cache_volume = cfg.buildStrategy.dockerCacheVolume;
    };

    container_registry = cfg.containerRegistry;

    provenance = cfg.provenance;
  };

  # `filterAttrsRecursive` does not descend into list ELEMENTS, so the matcher
  # attrsets nested inside `tier_rules` / `export_targets` and the per-phase
  # records inside `phases` would keep their nulls. Walk the whole tree.
  stripNulls = v:
    if lib.isAttrs v && !lib.isDerivation v
    then lib.mapAttrs (_: stripNulls) (lib.filterAttrs (_: x: x != null) v)
    else if lib.isList v
    then map stripNulls v
    else v;

  cleaned = stripNulls manifest;
in
(pkgs.formats.json { }).generate "ci-manifest.json" cleaned
