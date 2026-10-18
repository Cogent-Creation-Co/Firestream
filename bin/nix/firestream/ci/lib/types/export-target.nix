# CI profile shared type: build-phase export target
# Copyright Firestream. MIT License.
#
# THE HARD CASE. `build_export_target` in
# src/util/firestream-ci/docs/defaults-reference.rs.txt is the single member of
# the deleted `defaults` module that most resisted becoming data: it combined
# prefix matching, suffix matching, an infix `contains` test, and a
# strip-prefix/strip-suffix transform feeding a NESTED destination path
# (`conceptdb-portal-wasm` -> `conceptdb-wasm/portal`). The plan flags it as
# "the main design risk".
#
# It decomposes into exactly three declarative parts, with no residue:
#
#   1. `match`                     — a matcher over the attribute LEAF.
#   2. `stripPrefix` / `stripSuffix` — produce `{stem}` from the leaf. A strip
#                                    that does not apply is a no-op, mirroring
#                                    the reference's `.unwrap_or(attr)`.
#   3. `dest` / `kind`             — destination template + artifact class.
#
# Rules are evaluated IN ORDER; the first whose matcher accepts the leaf wins.
# Order is therefore part of the contract — the emitter preserves list order
# and never sorts.
#
# ── Template vocabulary in `dest` ────────────────────────────────────────────
#   {leaf}     the attribute leaf, verbatim
#   {stem}     the leaf after stripPrefix/stripSuffix
#   {arch}     container/target arch
#   {system}   Nix system double
#   {project}  project name
#
# `{leaf}` / `{stem}` are a narrow, deliberate extension to the plan's
# "{system}/{arch} only" rule, confined to `dest`. They are the rule's own
# captures — the declarative equivalent of a regex backreference — not project
# knowledge. Without them, the wasm case would need one rule per artifact,
# duplicating the project's artifact inventory. The invariant the plan is
# protecting (no project reasoning in Rust) is untouched: the resolver still
# only matches, strips, and substitutes.
#
# ── The four reference cases, transcribed ────────────────────────────────────
#   image   { match.prefix = "P-"; match.contains = "-linux-"; dest = "{leaf}"; }
#   sbom    { match.equals = "P-sbom";  dest = "sbom"; }
#   binary  { match.equals = "P-cli";   dest = "{leaf}-{arch}"; }
#   wasm    { match.prefix = "P-"; match.suffix = "-wasm";
#             stripPrefix = "P-"; stripSuffix = "-wasm";
#             dest = "P-wasm/{stem}"; }
#   (else)  exportDefault = { dest = "{leaf}"; kind = "binary"; }
#
# The end-to-end proof that this reproduces the reference exactly, from JSON
# alone and for a project that is not Firestream, is
# src/util/firestream-ci/tests/profile_fixtures.rs.

{ lib, ... }:

let
  inherit (lib) mkOption types;
  matcher = import ./matcher.nix { inherit lib; };

  # Mirrors `firestream_ci::profile::spec::KNOWN_ARTIFACT_KINDS` and
  # `firestream_ci::manifest::ArtifactKind`. Kept an enum so a typo
  # (`imgae`) fails at eval time rather than becoming
  # `ArtifactKind::Other("imgae")` in a manifest six months later.
  artifactKind = types.enum [
    "image"
    "container"
    "binary"
    "wasm"
    "sbom"
    "app"
    "nix"
  ];

in {
  inherit artifactKind;

  exportTargetType = types.submodule {
    options = {
      match = mkOption {
        type = matcher.matcherType;
        default = { };
        description = ''
          Matcher over the attribute leaf. All populated clauses must hold.
          An unset matcher matches everything — use it only as a terminal
          catch-all, and prefer `exportDefault` for that.
        '';
      };

      stripPrefix = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Removed from the front of the leaf to produce `{stem}`.";
        example = "firestream-";
      };

      stripSuffix = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Removed from the end of the leaf to produce `{stem}`.";
        example = "-wasm";
      };

      dest = mkOption {
        type = types.str;
        description = ''
          Destination sub-path under `<rundir>/artifacts/`. Expands
          `{leaf}`, `{stem}`, `{arch}`, `{system}`, `{project}`. May contain
          `/` to nest.
        '';
        example = "firestream-wasm/{stem}";
      };

      kind = mkOption {
        type = artifactKind;
        description = "Artifact class recorded in the run manifest.";
        example = "image";
      };
    };
  };

  exportDefaultType = types.submodule {
    options = {
      dest = mkOption {
        type = types.str;
        default = "{leaf}";
        description = "Destination template when no `exportTargets` rule matches.";
      };
      kind = mkOption {
        type = artifactKind;
        default = "binary";
        description = "Artifact class when no `exportTargets` rule matches.";
      };
    };
  };
}
