# Kafka chart options aggregator (Phase 6b - Agent G3)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports every per-section options module (Phase 6b), each declaring
# `options.kafka.<section>.*`, and projects the resolved option tree into
# the engine-declared `config.kafka.values` attrset that the values.yaml
# emitter serialises.
#
# Kafka's Bitnami chart is pure KRaft (Apache Kafka 4.x); there is no
# zookeeper subchart dependency. Only `common` is vendored.
#
# CRITICAL — the `config` argument MUST be bound in the module signature
# below. `builtins.removeAttrs config.kafka [ "_meta" "values" ]` strips
# those two keys BEFORE the recursive filter runs. `_meta`/`values` are
# declared by the engine's stdSchema, so leaving `values` in the tree
# would make `config.kafka.values` reference itself (infinite recursion).
# We read every OTHER section, drop nulls recursively, and write the
# result back to `values`.
{ lib, config, ... }:

{
  imports = [
    # Common / top-level
    ./options/common.nix
    ./options/global.nix

    # Core Kafka image + cluster config
    ./options/image.nix
    ./options/kraft.nix
    ./options/kafka-config.nix

    # Listeners / auth / encryption
    ./options/listeners.nix
    ./options/sasl.nix
    ./options/tls.nix

    # Default init containers (volume-permissions, prepare-config, auto-discovery)
    ./options/default-init-containers.nix

    # StatefulSet shapes
    ./options/controller.nix
    ./options/broker.nix

    # Networking / services / external access
    ./options/service.nix
    ./options/external-access.nix
    ./options/network-policy.nix

    # Security / access
    ./options/service-account.nix
    ./options/rbac.nix
    ./options/service-bindings.nix

    # Observability / topics
    ./options/metrics.nix
    ./options/provisioning.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: Kafka benefits from atomic + wait so a failed install
  #   rolls back cleanly. 10m timeout accommodates PVC provisioning + image
  #   pulls + KRaft cluster bootstrap. Same shape as redis / postgresql
  #   defaults.
  # - lifecycle.dependsOn: Kafka is a leaf dependency in firestreamStacks
  #   (other charts depend on it, not the other way round).
  # - lifecycle.lastBreakingVersion: no Rust-side breaking-version handler
  #   wired up yet (helm_lifecycle/charts/kafka.rs uses the Strimzi operator
  #   path; the Bitnami chart seam is not gated on chart version). Leaving
  #   null until that lifecycle plumbing catches up.
  # - containerRefs: no firestreamImages.kafka injection yet (Bitnami's
  #   bundled `bitnami/kafka` + `bitnami/jmx-exporter` + `bitnami/kubectl` +
  #   `bitnami/os-shell` images render with their defaults); leaving empty
  #   so the chart renders unchanged.
  # - provenance: framework.nix does not yet plumb inputs.self.rev into
  #   evalChart; both fields stay null.
  # ------------------------------------------------------------------
  config.kafka._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "15m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.kafka._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # No firestream container override yet; the chart renders with Bitnami's
  # bundled `bitnami/kafka*` images. Image-injection becomes opt-in once
  # firestreamImages.kafka lands.
  config.kafka._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`)
  # into the serialised `values` attrset, stripping null leaves recursively.
  # The emitter (to-values-yaml.nix) does its own null-strip too, but
  # filtering here keeps `config.kafka.values` a faithful, null-free mirror
  # of the overrides.
  config.kafka.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.kafka [ "_meta" "values" ]);
}
