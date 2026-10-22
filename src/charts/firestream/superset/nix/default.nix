# Superset chart options aggregator (Phase 6b - Agent G6)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports every per-section options module, each declaring
# `options.superset.<section>.*`, and projects the resolved option tree into
# the engine-declared `config.superset.values` attrset that the values.yaml
# emitter serialises.
#
# Superset is a hub-and-spoke Bitnami chart: a main web pod plus celery
# worker / beat / flower siblings, with bundled `postgresql` and `redis`
# subcharts (Chart.yaml v4.0.1 lists both). `common` is also vendored. The
# externalDatabase / externalRedis tops give the fallback path when the
# bundled subcharts are disabled.
#
# Quirks worth documenting up-front:
#  * `auth.secretKey` AND `auth.password` are randomised per render by
#    `common.secrets.passwords.manage` when left empty (`secret.yaml` writes
#    `superset-password` / `superset-secret-key`).
#  * The Bitnami chart computes the SQLAlchemy connection URI at template
#    time from `postgresql.auth.*` / `externalDatabase.*` — no special
#    knob, but the `superset_config.py` carries a `SQLALCHEMY_DATABASE_URI`
#    that hashes into the configmap's `checksum/configuration` annotation.
#
# CRITICAL — the `config` argument MUST be bound in the module signature
# below. `builtins.removeAttrs config.superset [ "_meta" "values" ]` strips
# those two keys BEFORE the recursive filter runs. `_meta`/`values` are
# declared by the engine's stdSchema, so leaving `values` in the tree
# would make `config.superset.values` reference itself (infinite recursion).
# We read every OTHER section, drop nulls recursively, and write the
# result back to `values`.
{ lib, config, ... }:

{
  imports = [
    # Top-level / cross-chart parameters
    ./options/common.nix
    ./options/global.nix

    # Main superset image (single registry/repository/tag triple used by web,
    # worker, beat, flower, init — Bitnami superset ships ONE image).
    ./options/image.nix

    # Admin/web auth: username/email/password/secretKey/existingSecret
    ./options/auth.nix

    # Application pod-specs
    ./options/web.nix
    ./options/worker.nix
    ./options/init.nix
    ./options/beat.nix
    ./options/flower.nix

    # Cluster-facing concerns
    ./options/ingress.nix
    ./options/default-init-containers.nix
    ./options/service-account.nix

    # Bundled subcharts (postgresql + redis) and their external fallbacks
    ./options/postgresql.nix
    ./options/external-database.nix
    ./options/redis.nix
    ./options/external-redis.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: same atomic + wait shape as jupyterhub / spark — superset
  #   has init jobs (DB init, examples loading) plus 4 application pods
  #   (web / worker / beat / flower), so --wait + waitForJobs + a 10m
  #   timeout cover image pulls + subchart rollouts.
  # - lifecycle.dependsOn: postgresql + redis are BUNDLED subcharts that
  #   deploy as part of the superset release itself; they are NOT external
  #   stack-level dependencies. The `firestreamStacks.dev` ordering of
  #   postgresql/redis before superset is for separate top-level instances,
  #   not for the bundled subcharts. dependsOn therefore stays empty.
  # - lifecycle.lastBreakingVersion: no Rust-side breaking-version handler
  #   exists at helm_lifecycle/charts/superset.rs yet.
  # - containerRefs: no firestreamImages.superset injection yet; the chart
  #   renders with Bitnami's bundled `bitnami/superset` image.
  # - provenance: framework.nix does not yet plumb inputs.self.rev into
  #   evalChart; both fields stay null.
  # ------------------------------------------------------------------
  config.superset._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.superset._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # No firestream container override yet; the chart renders with Bitnami's
  # bundled `bitnami/superset` image. Image-injection becomes opt-in once
  # firestreamImages.superset lands.
  config.superset._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`)
  # into the serialised `values` attrset, stripping null leaves recursively.
  # The emitter (to-values-yaml.nix) does its own null-strip too, but
  # filtering here keeps `config.superset.values` a faithful, null-free
  # mirror of the overrides.
  config.superset.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.superset [ "_meta" "values" ]);
}
