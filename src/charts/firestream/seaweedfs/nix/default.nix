# SeaweedFS chart options aggregator (Phase C — object store).
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via
# `modules = [ ./nix/default.nix ]`). It imports the per-section option modules,
# sets the Firestream DEFAULT overrides that turn the all-in-one + S3 topology
# on, and projects the resolved option tree into `config.seaweedfs.values`.
#
# CRITICAL — the `config` argument MUST be bound in the module signature below.
# `builtins.removeAttrs config.seaweedfs [ "_meta" "values" ]` strips those two
# engine-declared keys BEFORE the recursive filter runs, otherwise
# `config.seaweedfs.values` would reference itself (infinite recursion).
#
# NOTE: SeaweedFS is NOT a Bitnami chart. The emitted values.yaml is an OVERLAY
# that `helm template -f` deep-merges on top of the chart's own values.yaml, so
# this module only declares the keys we actually override; everything else
# (master/volume/filer ports, probes, etc.) passes through from upstream.
{ lib, config, ... }:

{
  imports = [
    ./options/image.nix
    ./options/global.nix
    ./options/s3.nix
    ./options/all-in-one.nix
    ./options/components.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: atomic + wait so a failed install rolls back cleanly. The
  #   post-install bucket hook is a Job, so wait-for-jobs is on. 10m timeout
  #   accommodates image pulls + the bucket hook's master/filer readiness wait.
  # - lifecycle.dependsOn: SeaweedFS is the object store — a leaf dependency
  #   other charts (spark/airflow) depend on, not the other way round.
  # ------------------------------------------------------------------
  config.seaweedfs._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.seaweedfs._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # ------------------------------------------------------------------
  # Firestream default overrides (the overlay that turns the object store on).
  # ------------------------------------------------------------------
  config.seaweedfs.global.seaweedfs.loggingLevel = 1;

  config.seaweedfs.allInOne = {
    enabled = true;
    data.type = "emptyDir";
    s3 = {
      enabled = true;
      enableAuth = true;
      createBuckets = [ { name = "firestream"; } ];
    };
  };

  # Single-pod topology: the all-in-one pod IS master+volume+filer+s3. Disable
  # the distributed StatefulSets (master/volume/filer) and the standalone s3
  # Deployment so we don't render a conflicting duplicate topology alongside the
  # all-in-one pod. The all-in-one pod still reads `.Values.{master,volume,
  # filer}.port` etc., which remain defined even with `enabled=false`.
  config.seaweedfs.master.enabled = false;
  config.seaweedfs.volume.enabled = false;
  config.seaweedfs.filer.enabled = false;

  # Top-level s3.* — the secret template (s3-secret.yaml) reads from here, not
  # from allInOne.s3, so auth + explicit credentials live here. `enabled=false`
  # turns OFF the standalone S3 gateway Deployment (the embedded S3 in the
  # all-in-one pod is gated by allInOne.s3.enabled instead); `port`/`credentials`
  # are still consumed by the all-in-one pod + the s3 secret, and the secret is
  # still generated because allInOne.enabled is true.
  config.seaweedfs.s3 = {
    enabled = false;
    port = 8333;
    enableAuth = true;
    domainName = "";
    credentials.admin = {
      accessKey = "firestream";
      secretKey = "firestream-secret";
    };
  };

  # Project the resolved option tree (minus the engine's `_meta`/`values`) into
  # the serialised `values` attrset, stripping null leaves recursively.
  config.seaweedfs.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.seaweedfs [ "_meta" "values" ]);
}
