# PostgreSQL chart options aggregator (Phase 6b - Agent G1)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports every per-section options module (Phase 6b), each declaring
# `options.postgresql.<section>.*`, and projects the resolved option tree into
# the engine-declared `config.postgresql.values` attrset that the values.yaml
# emitter serialises.
#
# CRITICAL — the `config` argument MUST be bound in the module signature below.
# `builtins.removeAttrs config.postgresql [ "_meta" "values" ]` strips those
# two keys BEFORE the recursive filter runs. `_meta`/`values` are declared by
# the engine's stdSchema, so leaving `values` in the tree would make
# `config.postgresql.values` reference itself (infinite recursion). We read
# every OTHER section, drop nulls recursively, and write the result back to
# `values`.
{ lib, config, ... }:

{
  imports = [
    # Common / top-level
    ./options/common.nix
    ./options/global.nix

    # Core PostgreSQL configuration
    ./options/image.nix
    ./options/auth.nix
    ./options/architecture.nix
    ./options/audit.nix
    ./options/ldap.nix
    ./options/tls.nix

    # StatefulSet shapes
    ./options/primary.nix
    ./options/read-replicas.nix

    # Operations
    ./options/volume-permissions.nix
    ./options/metrics.nix
    ./options/backup.nix
    ./options/password-update-job.nix

    # Security and access
    ./options/service-account.nix
    ./options/rbac.nix
    ./options/service-bindings.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: PostgreSQL benefits from atomic + wait so a failed install
  #   rolls back cleanly. 10m timeout accommodates PVC provisioning + image
  #   pulls. Same shape as airflow's defaults.
  # - lifecycle.dependsOn: PostgreSQL is a leaf dependency in firestreamStacks
  #   (other charts depend on it, not the other way round).
  # - lifecycle.lastBreakingVersion: a postgresql breaking-version handler does
  #   exist in src/lib/rust/firestream/src/deploy/helm_lifecycle/charts/, but
  #   wiring it through the chart manifest is out of scope for this phase.
  #   Leaving null until Rust-side lifecycle plumbing catches up.
  # - containerRefs: no firestreamImages.postgresql yet (see Agent B's
  #   hand-off); leaving empty so the chart renders with Bitnami's defaults.
  # - provenance: framework.nix does not yet plumb inputs.self.rev into
  #   evalChart; both fields stay null.
  # ------------------------------------------------------------------
  config.postgresql._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.postgresql._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # No firestream container override yet; the chart renders with Bitnami's
  # bundled `bitnami/postgresql` image. Image-injection becomes opt-in once
  # firestreamImages.postgresql lands.
  config.postgresql._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`) into
  # the serialised `values` attrset, stripping null leaves recursively. The
  # emitter (to-values-yaml.nix) does its own null-strip too, but filtering here
  # keeps `config.postgresql.values` a faithful, null-free mirror of the
  # overrides.
  config.postgresql.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.postgresql [ "_meta" "values" ]);
}
