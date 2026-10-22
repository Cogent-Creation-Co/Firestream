# Redis chart options aggregator (Phase 6b - Agent G2)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports every per-section options module (Phase 6b), each declaring
# `options.redis.<section>.*`, and projects the resolved option tree into
# the engine-declared `config.redis.values` attrset that the values.yaml
# emitter serialises.
#
# CRITICAL — the `config` argument MUST be bound in the module signature below.
# `builtins.removeAttrs config.redis [ "_meta" "values" ]` strips those
# two keys BEFORE the recursive filter runs. `_meta`/`values` are declared by
# the engine's stdSchema, so leaving `values` in the tree would make
# `config.redis.values` reference itself (infinite recursion). We read
# every OTHER section, drop nulls recursively, and write the result back to
# `values`.
{ lib, config, ... }:

{
  imports = [
    # Common / top-level
    ./options/common.nix
    ./options/global.nix

    # Core Redis configuration
    ./options/image.nix
    ./options/auth.nix
    ./options/architecture.nix
    ./options/tls.nix

    # StatefulSet shapes
    ./options/master.nix
    ./options/replica.nix
    ./options/sentinel.nix

    # Operations
    ./options/volume-permissions.nix
    ./options/sysctl.nix
    ./options/kubectl.nix
    ./options/metrics.nix

    # Security and access
    ./options/service-account.nix
    ./options/rbac.nix
    ./options/pod-security-policy.nix
    ./options/network-policy.nix
    ./options/service-bindings.nix
    ./options/use-external-dns.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: Redis benefits from atomic + wait so a failed install
  #   rolls back cleanly. 10m timeout accommodates PVC provisioning + image
  #   pulls. Same shape as postgresql / airflow defaults.
  # - lifecycle.dependsOn: Redis is a leaf dependency in firestreamStacks
  #   (other charts depend on it, not the other way round).
  # - lifecycle.lastBreakingVersion: no Rust-side breaking-version handler
  #   wired up yet — leaving null until the lifecycle plumbing catches up.
  # - containerRefs: no firestreamImages.redis yet (Bitnami's redis +
  #   redis-sentinel + redis-exporter images render with their defaults);
  #   leaving empty so the chart renders unchanged.
  # - provenance: framework.nix does not yet plumb inputs.self.rev into
  #   evalChart; both fields stay null.
  # ------------------------------------------------------------------
  config.redis._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.redis._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # No firestream container override yet; the chart renders with Bitnami's
  # bundled `bitnami/redis*` images. Image-injection becomes opt-in once
  # firestreamImages.redis lands.
  config.redis._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`) into
  # the serialised `values` attrset, stripping null leaves recursively. The
  # emitter (to-values-yaml.nix) does its own null-strip too, but filtering here
  # keeps `config.redis.values` a faithful, null-free mirror of the
  # overrides.
  config.redis.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.redis [ "_meta" "values" ]);
}
