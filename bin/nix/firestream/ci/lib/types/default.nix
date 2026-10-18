# CI Profile Type Definitions
# Copyright Firestream. MIT License.
#
# The CI-profile mirror of bin/nix/firestream/charts/lib/types/default.nix.
# Exports every reusable type for `ci-manifest.json`'s typed option schema, and
# is injected into per-profile option modules through eval-ci.nix's specialArgs
# as `ciTypes` (exactly as `chartTypes` is injected into chart overlays), so a
# profile module writes:
#
#   { lib, ciTypes, ... }: let t = ciTypes; in { ... }
#
# without counting `../` hops.
#
# ── Model note: A vs "Model B" ───────────────────────────────────────────────
# The chart types follow "Model A": every leaf is `nullOr`-wrapped and defaults
# to null, because a chart's generated values.yaml must be a SPARSE OVERRIDE
# over the chart's own bundled defaults — a baked default there would silently
# diverge from upstream.
#
# The CI profile is the opposite situation: `ci-manifest.json` is the COMPLETE
# document, not an overlay on anything. So concrete leaves carry real defaults
# (empty lists, `"auto"`, the 100 MiB size floor) and `nullOr` is reserved for
# the handful of fields whose absence is genuinely meaningful — the matcher
# clauses, and the two "derive it from `project.name`" prefixes. The emitter
# (../to-ci-manifest.nix) strips nulls, so those come through as absent keys
# and the Rust reader applies its documented fallback.

{ lib, ... }:

let
  matcher = import ./matcher.nix { inherit lib; };
  phase = import ./phase.nix { inherit lib; };
  exportTarget = import ./export-target.nix { inherit lib; };
  builder = import ./builder.nix { inherit lib; };
  project = import ./project.nix { inherit lib; };

in {
  inherit (matcher) matcherType tierType;
  inherit (phase) phaseType;
  inherit (exportTarget) exportTargetType exportDefaultType artifactKind;
  inherit (builder) builderType buildStrategyType envSentinelType;
  inherit (project) projectType;
}
