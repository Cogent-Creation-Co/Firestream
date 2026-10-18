# CI profile shared type: declarative string matcher
# Copyright Firestream. MIT License.
#
# The matching vocabulary for BOTH `tierRules` and `exportTargets`. Every
# populated clause must hold (logical AND); an entirely unset matcher matches
# everything, which is how a catch-all terminal rule is written.
#
# Scope discipline: prefix / suffix / contains / equals and nothing else. That
# is exactly the set the reference `build_export_target`
# (src/util/firestream-ci/docs/defaults-reference.rs.txt) needed — `starts_with`,
# `ends_with`, `contains`, `==`. Globs and regex are deliberately absent: the
# reference used neither, and adding one would put a matching engine's
# semantics (which flavour? which anchoring?) into the JSON contract.
#
# Model A (see ../../../charts/lib/types/default.nix): every leaf is
# `nullOr`-wrapped and defaults to `null`, so an unset clause is stripped by
# the manifest emitter rather than serialised as an empty string that the Rust
# reader would have to special-case.

{ lib, ... }:

let
  inherit (lib) mkOption types;

in {
  matcherType = types.submodule {
    options = {
      equals = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Exact match against the attribute leaf.";
        example = "firestream-sbom";
      };

      prefix = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "The leaf must start with this string.";
        example = "required-";
      };

      suffix = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "The leaf must end with this string.";
        example = "-chart";
      };

      contains = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "The leaf must contain this substring anywhere.";
        example = "-linux-";
      };
    };
  };

  # Tier enum, spelled the way the JSON contract and `firestream_ci::pipeline::Tier`
  # spell it (lowercase).
  tierType = types.enum [ "required" "advisory" ];
}
