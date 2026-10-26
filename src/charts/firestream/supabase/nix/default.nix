# Supabase chart options aggregator.
# Copyright Firestream. MIT License.
#
# Consumed as a module by `evalChart` (nix/flake-modules/charts/supabase.nix).
# It imports the per-domain option modules, sets the chart-manifest metadata,
# and projects the resolved option tree into `config.supabase.values`.
#
# The chart is a Firestream fork of supabase-community/supabase-kubernetes and
# its values.yaml already carries Firestream defaults, so the emitted
# values.yaml is a sparse overlay: image injection and the development key set
# (both set by the flake-module) and any consumer override.
#
# The `config` argument must be bound: `values` strips `_meta` and itself
# before the null filter, or it would refer to itself.
{ lib, config, ... }:

{
  imports = [
    ./options/secret.nix
    ./options/components.nix
    ./options/cluster.nix
  ];

  # NOTE: the first database boot runs every Supabase migration, and the
  # Storage init-bucket container waits for the SeaweedFS S3 endpoint. Both
  # sit inside the --wait window, so the timeout is longer than the default.
  config.supabase._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = false;
    timeout = "15m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  # Storage keeps its objects in the SeaweedFS release of the same stack.
  config.supabase._meta.lifecycle = {
    dependsOn = [ "seaweedfs" ];
    lastBreakingVersion = null;
  };

  config.supabase.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.supabase [ "_meta" "values" ]);
}
