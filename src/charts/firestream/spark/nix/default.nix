# Spark chart options aggregator (Phase 6b - Agent G4)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports every per-section options module (Phase 6b), each declaring
# `options.spark.<section>.*`, and projects the resolved option tree into
# the engine-declared `config.spark.values` attrset that the values.yaml
# emitter serialises.
#
# The Bitnami spark chart is plain master/worker StatefulSets (NOT the
# Kubernetes spark-operator). Only `common` is vendored.
#
# CRITICAL — the `config` argument MUST be bound in the module signature
# below. `builtins.removeAttrs config.spark [ "_meta" "values" ]` strips
# those two keys BEFORE the recursive filter runs. `_meta`/`values` are
# declared by the engine's stdSchema, so leaving `values` in the tree
# would make `config.spark.values` reference itself (infinite recursion).
# We read every OTHER section, drop nulls recursively, and write the
# result back to `values`.
{ lib, config, ... }:

{
  imports = [
    # Common / top-level
    ./options/common.nix
    ./options/global.nix

    # Core Spark image
    ./options/image.nix

    # StatefulSet shapes (master/worker)
    ./options/master.nix
    ./options/worker.nix

    # Security (RPC auth, SSL/JKS)
    ./options/security.nix

    # Networking / services / ingress
    ./options/service.nix
    ./options/ingress.nix

    # Service account (no separate top-level rbac/networkPolicy: those
    # live on master.networkPolicy / worker.networkPolicy in this chart)
    ./options/service-account.nix

    # Observability
    ./options/metrics.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: same atomic + wait shape as postgresql / redis / kafka.
  #   Spark needs --wait so master + workers reach Ready before downstream
  #   stacks (airflow, jupyterhub) try to submit jobs. 10m timeout covers
  #   image pulls + StatefulSet ordered rollouts.
  # - lifecycle.dependsOn: leaf dependency; nothing in firestreamStacks
  #   currently lists spark as a prerequisite for itself.
  # - lifecycle.lastBreakingVersion: no Rust-side breaking-version handler
  #   exists at helm_lifecycle/charts/spark.rs (the chart isn't even
  #   referenced from there). Leaving null until that lifecycle plumbing
  #   catches up.
  # - containerRefs: no firestreamImages.spark injection yet (Bitnami's
  #   bundled `bitnami/spark` image renders with its default); leaving
  #   empty so the chart renders unchanged.
  # - provenance: framework.nix does not yet plumb inputs.self.rev into
  #   evalChart; both fields stay null.
  # ------------------------------------------------------------------
  config.spark._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.spark._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # No firestream container override yet; the chart renders with Bitnami's
  # bundled `bitnami/spark` image. Image-injection becomes opt-in once
  # firestreamImages.spark lands.
  config.spark._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`)
  # into the serialised `values` attrset, stripping null leaves recursively.
  # The emitter (to-values-yaml.nix) does its own null-strip too, but
  # filtering here keeps `config.spark.values` a faithful, null-free mirror
  # of the overrides.
  config.spark.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.spark [ "_meta" "values" ]);
}
