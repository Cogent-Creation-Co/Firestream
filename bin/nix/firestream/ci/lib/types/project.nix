# CI profile shared type: project identity
# Copyright Firestream. MIT License.
#
# `name` is the `{project}` expansion source and the stem every derived prefix
# falls back to. `nixSystem` / `arch` are the values THIS manifest was emitted
# for — the manifest is per-system by design (that is the whole arch-gating
# mechanism), so they are not "defaults" so much as the document's own identity.

{ lib, ... }:

let
  inherit (lib) mkOption types;

in {
  projectType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = "Short project slug. Expanded as `{project}`.";
        example = "firestream";
      };

      nixSystem = mkOption {
        type = types.str;
        description = "Nix system double this manifest was emitted for.";
        example = "x86_64-linux";
      };

      arch = mkOption {
        type = types.str;
        description = "Container/target arch this manifest was emitted for.";
        example = "x86_64";
      };

      k8sNamespacePrefix = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Prefix for `firestream-ci k8s namespace` output (branch -> namespace).
          `null` => the consumer derives `"<name>-"`.
        '';
        example = "firestream-";
      };
    };
  };
}
