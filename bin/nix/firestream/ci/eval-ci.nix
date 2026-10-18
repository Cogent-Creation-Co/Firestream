# CI Profile Evaluation Mechanism
# Copyright Firestream. MIT License.
#
# Options-driven entrypoint for evaluating a CI profile and emitting the
# `ci-manifest.json` payload that `firestream_ci::profile` reads at runtime.
#
# It is the one-for-one mirror of the chart evaluator
# (bin/nix/firestream/charts/eval-chart.nix), which is itself the mirror of the
# container evaluator. Same shape, same idiom, same contract:
#
#     typed options  ->  lib.evalModules  ->  JSON derivation  ->  Rust reader
#
#   charts/eval-chart.nix       ci/eval-ci.nix
#   charts/lib/types/*          ci/lib/types/*
#   charts/lib/to-chart-manifest.nix   ci/lib/to-ci-manifest.nix
#   chart-manifest.json         ci-manifest.json
#   firestream-charts crate     firestream_ci::profile
#   FIRESTREAM_CHARTS_DIR       FIRESTREAM_CI_PROFILE
#
# WHY THIS EXISTS: ConceptDB's `oxi-ci` carried its project knowledge as
# compiled-in Rust behind a cargo feature (`src/defaults/mod.rs`, preserved at
# src/util/firestream-ci/docs/defaults-reference.rs.txt). The Firestream lift
# deleted it. Everything it encoded — check attr lists, build attr lists, tier
# classification, export targets, builder identity, the passthrough allowlist —
# is declared here instead, as data. There is no project-specific Rust.
#
# THE INVARIANT: `{system}` / `{arch}` / `{project}` are the entire templating
# vocabulary (plus `{leaf}` / `{stem}` inside an export rule's `dest`).
# Conditional attribute sets — the reference's x86_64-only GPU package is the
# canonical case — are expressed by THIS FILE being evaluated once per system
# and emitting a system-specific manifest. Nothing downstream branches on arch.
#
# Signature:
#   { pkgs, lib }:
#     { modules ? [], nixSystem ? pkgs.system, arch ? <derived>, provenance ? {} }:
#
# Returns:
#   { manifest; profileDir; config; options; }
#     manifest    - the ci-manifest.json derivation itself
#     profileDir  - a directory containing `ci-manifest.json`, so a consumer can
#                   point FIRESTREAM_CI_PROFILE at either the file or the dir
#                   (the reader accepts both, mirroring FIRESTREAM_CHARTS_DIR)
{ pkgs, lib }:

{
  modules ? [ ],
  nixSystem ? pkgs.stdenv.hostPlatform.system,
  # Container/target arch: the CPU half of the Nix double. `x86_64-linux` ->
  # `x86_64`, `aarch64-darwin` -> `aarch64`. Split rather than matched so an
  # unusual system yields something legible instead of an eval error.
  arch ? lib.head (lib.splitString "-" nixSystem),
  provenance ? { },
  # Names of the flake's `checks.<system>` attributes, so a profile can DERIVE
  # its verify list instead of hand-transcribing it (which had already drifted:
  # 13 gated vs 23 declared). Passed through specialArgs as `checkNames`.
  # Defaults to [] so a consumer that does not care — or a bare `evalCi` call in
  # a test — still evaluates.
  checkNames ? [ ],
}:

let
  # Shared, profile-agnostic types. Injected via specialArgs as `ciTypes`, the
  # exact seam eval-chart.nix provides as `chartTypes`.
  ciTypes = import ./lib/types { inherit lib; };

  # The standard option schema. Unlike eval-chart.nix — where the ~3000 typed
  # option paths live in each chart's own option modules — the CI schema is
  # small and closed, so it is declared here in full and profile modules only
  # supply VALUES. That is the right split: a chart's shape varies per chart,
  # a CI profile's shape does not vary per project.
  mkCiOptions = { lib, ... }: {
    options.ci = {
      project = lib.mkOption {
        type = ciTypes.projectType;
        description = "Project identity: name, and the system/arch this manifest is emitted for.";
        default = {
          name = "unnamed";
          nixSystem = nixSystem;
          arch = arch;
        };
      };

      # Declared as an attrset (so multiple modules can contribute to the same
      # phase by name, the usual module-system merge) and flattened to an
      # ordered list by the emitter using each phase's `order`.
      phases = lib.mkOption {
        type = lib.types.attrsOf ciTypes.phaseType;
        default = { };
        description = "Pipeline phases, keyed by name. Emitted as an array ordered by `order`.";
      };

      tierRules = lib.mkOption {
        type = lib.types.listOf (lib.types.submodule {
          options = {
            match = lib.mkOption {
              type = ciTypes.matcherType;
              default = { };
              description = "Matcher over the attribute leaf.";
            };
            tier = lib.mkOption {
              type = ciTypes.tierType;
              description = "Tier assigned when the matcher accepts.";
            };
          };
        });
        default = [ ];
        description = ''
          Ordered attr-leaf -> tier classification rules; FIRST MATCH WINS, so
          list order is part of the contract. A project whose check names carry
          no tier convention leaves this empty and uses each phase's
          `advisoryAttrs` instead.
        '';
      };

      tierDefault = lib.mkOption {
        type = ciTypes.tierType;
        default = "required";
        description = ''
          Tier when no rule matches. `required` on purpose: an unintentional
          advisory typo must not silently downgrade a gate.
        '';
      };

      exportTargets = lib.mkOption {
        type = lib.types.listOf ciTypes.exportTargetType;
        default = [ ];
        description = "Ordered build-phase export rules; first match wins. See lib/types/export-target.nix.";
      };

      exportDefault = lib.mkOption {
        type = ciTypes.exportDefaultType;
        default = { };
        description = "Terminal fallback when no `exportTargets` rule matches.";
      };

      passthroughVars = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = ''
          Env vars forwarded from the host into the builder container.
          `FIRESTREAM_CI_PROFILE` is appended by the consumer whether or not it
          appears here — the inner run must resolve the same manifest as its
          parent.
        '';
      };

      devshellSentinels = lib.mkOption {
        type = lib.types.attrsOf ciTypes.envSentinelType;
        default = { };
        description = "Env probes proving 'we are inside this project's Nix devshell'. Any one satisfied is enough.";
      };

      builder = lib.mkOption {
        type = ciTypes.builderType;
        default = { };
        description = "Builder-image identity, base image, size floor, container naming.";
      };

      buildStrategy = lib.mkOption {
        type = ciTypes.buildStrategyType;
        default = { };
        description = "Native-vs-docker build and cache policy (Part C of the plan).";
      };

      containerRegistry = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = { };
        example = { "redis:" = "redis-7"; "redis:8" = "redis-8"; };
        description = ''
          `<container>:<version>` -> Nix package attribute name. An empty
          version is that container's DEFAULT, and resolution is exactly
          `bin/build/_common.sh::resolve_package_name`: try `<c>:<v>`, then
          fall back to `<c>:`, then fail.

          This is a table, not a convention: `.#redis` is redis-8 in
          nix/flake-modules/containers/redis.nix while a bare `redis` on the
          build path must stay redis-7, and `odoo:` resolves to the
          UNSUFFIXED `odoo`. Deriving the name from the container directory
          would silently change which image gets built.

          Consumed by `firestream_ci::profile::Profile::resolve_package_name`.
          Nothing is compiled into Rust.
        '';
      };

      provenance = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = { };
        description = "Free-form build provenance (flake rev, nixpkgs rev). Never interpreted by the consumer.";
      };
    };
  };

  # `nixSystem` / `arch` / `provenance` arrive as arguments rather than options
  # so a caller instantiating the profile per system does not have to thread
  # them through every module. They are applied as a LOW-priority default so a
  # profile module can still override.
  systemModule = { lib, ... }: {
    config.ci = {
      project = {
        nixSystem = lib.mkDefault nixSystem;
        arch = lib.mkDefault arch;
      };
      provenance = lib.mkDefault provenance;
    };
  };

  evaled = lib.evalModules {
    modules = [ mkCiOptions systemModule ] ++ modules;
    specialArgs = { inherit pkgs lib ciTypes checkNames; };
  };

  cfg = evaled.config.ci;

  manifest = (import ./lib/to-ci-manifest.nix { inherit pkgs lib; }) cfg;

  # A directory whose only content is the manifest. `FIRESTREAM_CI_PROFILE` may
  # point at either this dir or the file inside it — `firestream_ci::profile`'s
  # resolver appends `ci-manifest.json` when handed a directory, mirroring how
  # FIRESTREAM_CHARTS_DIR points at the chart farm root.
  #
  # Build-gated: `jq` re-parses the emitted JSON and asserts `schema_version`,
  # so a malformed emitter change fails at `nix build` time rather than at the
  # first CI run. This is the CI-profile analogue of eval-chart.nix's in-sandbox
  # `helm template` gate.
  profileDir = pkgs.runCommand "firestream-ci-profile"
    {
      nativeBuildInputs = [ pkgs.jq ];
      meta = {
        description = "CI profile payload (ci-manifest.json) for firestream-ci";
        platforms = lib.platforms.all;
      };
    } ''
    set -euo pipefail
    mkdir -p "$out"
    cp ${manifest} "$out/ci-manifest.json"

    jq -e '.schema_version == 1' "$out/ci-manifest.json" > /dev/null \
      || { echo "ci-manifest.json: schema_version must be 1" >&2; exit 1; }
    jq -e '.project.name | type == "string" and length > 0' "$out/ci-manifest.json" > /dev/null \
      || { echo "ci-manifest.json: project.name must be a non-empty string" >&2; exit 1; }
    jq -e '.phases | type == "array"' "$out/ci-manifest.json" > /dev/null \
      || { echo "ci-manifest.json: phases must be an array" >&2; exit 1; }
  '';

in {
  inherit manifest profileDir;
  config = evaled.config;
  options = evaled.options;
}
