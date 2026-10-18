# Odoo chart options aggregator (Phase 6b - Agent G7)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports every per-section options module, each declaring
# `options.odoo.<section>.*`, and projects the resolved option tree into
# the engine-declared `config.odoo.values` attrset that the values.yaml
# emitter serialises.
#
# Odoo is the LAST chart wired in this milestone. Bitnami's odoo chart
# has a flatter shape than jupyterhub/superset: the main odoo Deployment's
# pod-spec (replicaCount, probes, resources, podSecurityContext, etc.)
# is at the TOP LEVEL of values.yaml rather than nested under e.g.
# `web.*`. We mirror that shape exactly — `app.nix` carries the top-level
# pod-spec/service/ingress/persistence/networkPolicy/pdb/autoscaling
# knobs, NOT a re-organised hierarchy. A bundled `postgresql` subchart
# (Chart.yaml v28.2.7 lists it under `postgresql.enabled`) supplies the
# database; no redis is bundled (Odoo doesn't use redis).
#
# Quirks worth documenting up-front:
#  * `auth.odooPassword` is randomised per render by
#    `common.secrets.passwords.manage` (length 10) when left empty
#    (`secrets.yaml` writes `odoo-password`). The `smtp-password` value
#    is base64-encoded from `smtpPassword` (NOT random — empty if unset).
#  * `loadDemoData` is a top-level boolean toggle, typically `false` in
#    dev; ensure it stays typed as such.
#  * `odooSkipInstall` is also a top-level boolean — the installation
#    wizard is skipped when true.
#  * `externalDatabase.postgresqlPostgresPassword` is a separate admin
#    password used by `externaldb-secrets.yaml` (only when
#    `postgresql.enabled = false` AND `externalDatabase.existingSecret`
#    is empty).
#
# CRITICAL — the `config` argument MUST be bound in the module signature
# below. `builtins.removeAttrs config.odoo [ "_meta" "values" ]` strips
# those two keys BEFORE the recursive filter runs. `_meta`/`values` are
# declared by the engine's stdSchema, so leaving `values` in the tree
# would make `config.odoo.values` reference itself (infinite recursion).
# We read every OTHER section, drop nulls recursively, and write the
# result back to `values`.
{ lib, config, ... }:

{
  imports = [
    # Top-level / cross-chart parameters
    ./options/common.nix
    ./options/global.nix

    # Main odoo image (single registry/repository/tag triple used by the
    # odoo Deployment — Bitnami odoo ships ONE image).
    ./options/image.nix

    # Admin user, email, password, SMTP credentials, existingSecret
    ./options/auth.nix

    # Init scripts run on first boot of the container
    ./options/init.nix

    # Main odoo Deployment pod-spec (replicaCount, probes, resources,
    # containerPorts, podSecurityContext, containerSecurityContext,
    # affinity, tolerations, autoscaling, pdb, service, ingress,
    # persistence, networkPolicy). This is FLAT (top-level keys in
    # values.yaml), not nested under `app.*`.
    ./options/app.nix

    # Cluster-facing concerns (separate sections in values.yaml)
    ./options/service.nix
    ./options/ingress.nix
    ./options/persistence.nix
    ./options/volume-permissions.nix
    ./options/service-account.nix
    ./options/network-policy.nix

    # Bundled subchart (postgresql) and external-database fallback
    ./options/postgresql.nix
    ./options/external-database.nix

    # Firestream-only: database + filestore backup CronJob. Not a Bitnami
    # value; the flake-module renders an `extraDeploy` CronJob that reads
    # `.Values.backup.*` at helm time.
    ./options/backup.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: same atomic + wait shape as jupyterhub / superset.
  #   Odoo has no init Job (initialisation happens inside the main
  #   container via customPostInitScripts), but the bundled postgresql
  #   subchart needs to be Ready before odoo starts, so --wait + a
  #   generous 10m timeout cover image pulls + subchart rollout. Odoo's
  #   first-boot initialisation (database install + module load) can be
  #   slow; the 600s livenessProbe initialDelay in upstream values
  #   reflects this.
  # - lifecycle.dependsOn: postgresql is BUNDLED as a subchart that
  #   deploys as part of the odoo release itself; it is NOT an external
  #   stack-level dependency. The `firestreamStacks.dev` ordering of
  #   postgresql before odoo is for separate top-level instances, not
  #   for the bundled subchart. dependsOn therefore stays empty.
  # - lifecycle.lastBreakingVersion: no Rust-side breaking-version
  #   handler exists at helm_lifecycle/charts/odoo.rs yet.
  # - containerRefs: no firestreamImages.odoo injection yet; the chart
  #   renders with Bitnami's bundled `bitnami/odoo` image.
  # - provenance: framework.nix does not yet plumb inputs.self.rev into
  #   evalChart; both fields stay null.
  # ------------------------------------------------------------------
  config.odoo._meta.deployment = {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.odoo._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # No firestream container override yet; the chart renders with
  # Bitnami's bundled `bitnami/odoo` image. Image-injection becomes
  # opt-in once firestreamImages.odoo lands.
  config.odoo._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`)
  # into the serialised `values` attrset, stripping null leaves recursively.
  # The emitter (to-values-yaml.nix) does its own null-strip too, but
  # filtering here keeps `config.odoo.values` a faithful, null-free
  # mirror of the overrides.
  config.odoo.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.odoo [ "_meta" "values" ]);
}
