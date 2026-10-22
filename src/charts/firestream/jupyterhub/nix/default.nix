# JupyterHub chart options aggregator (Phase 6b - Agent G5)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports every per-section options module, each declaring
# `options.jupyterhub.<section>.*`, and projects the resolved option tree into
# the engine-declared `config.jupyterhub.values` attrset that the values.yaml
# emitter serialises.
#
# The Bitnami JupyterHub chart bundles a vendored `postgresql` subchart (the
# hub uses Postgres by default; SQLite is supported by upstream z2jh but the
# Bitnami fork hard-codes a Postgres connection string in
# `hub.configuration`). `common` is also vendored.
#
# CRITICAL — the `config` argument MUST be bound in the module signature
# below. `builtins.removeAttrs config.jupyterhub [ "_meta" "values" ]` strips
# those two keys BEFORE the recursive filter runs. `_meta`/`values` are
# declared by the engine's stdSchema, so leaving `values` in the tree
# would make `config.jupyterhub.values` reference itself (infinite recursion).
# We read every OTHER section, drop nulls recursively, and write the
# result back to `values`.
{ lib, config, ... }:

{
  imports = [
    # Top-level / cross-chart parameters
    ./options/common.nix
    ./options/global.nix

    # Main hub controller + proxy front-door
    ./options/hub.nix
    ./options/proxy.nix

    # Pre-puller DaemonSet
    ./options/image-puller.nix

    # Per-user notebook spawner (KubeSpawner)
    ./options/singleuser.nix

    # Helper image used by init containers
    ./options/auxiliary-image.nix

    # Database: bundled subchart or external connection
    ./options/postgresql.nix
    ./options/external-database.nix
  ];

  # ------------------------------------------------------------------
  # chart-manifest.json metadata.
  #
  # - deployment: same atomic + wait shape as postgresql / redis / kafka /
  #   spark. JupyterHub depends on Postgres being Ready (hub_db_url is
  #   evaluated at hub startup) and on the proxy being Ready before users
  #   can route, so --wait + a generous 10m timeout cover image pulls +
  #   subchart rollouts.
  # - lifecycle.dependsOn: postgresql is the only external dependency
  #   (the bundled subchart handles the in-chart case; declaring it
  #   here also covers the `postgresql.enabled = false` -> external
  #   path, where downstream stacks still need the Postgres instance
  #   up first).
  # - lifecycle.lastBreakingVersion: no Rust-side breaking-version
  #   handler exists at helm_lifecycle/charts/jupyterhub.rs (the chart
  #   isn't referenced from there). Leaving null until that lifecycle
  #   plumbing catches up.
  # - containerRefs: no firestreamImages.jupyterhub injection yet; the
  #   chart renders with Bitnami's bundled
  #   `bitnami/jupyterhub`/`bitnami/configurable-http-proxy`/
  #   `bitnami/jupyter-base-notebook`/`bitnami/os-shell` images.
  # - provenance: framework.nix does not yet plumb inputs.self.rev into
  #   evalChart; both fields stay null.
  # ------------------------------------------------------------------
  config.jupyterhub._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.jupyterhub._meta.lifecycle = {
    dependsOn = [ "postgresql" ];
    lastBreakingVersion = null;
  };

  # No firestream container override yet; the chart renders with Bitnami's
  # bundled images. Image-injection becomes opt-in once
  # firestreamImages.jupyterhub lands.
  config.jupyterhub._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`)
  # into the serialised `values` attrset, stripping null leaves recursively.
  # The emitter (to-values-yaml.nix) does its own null-strip too, but
  # filtering here keeps `config.jupyterhub.values` a faithful, null-free
  # mirror of the overrides.
  config.jupyterhub.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.jupyterhub [ "_meta" "values" ]);
}
