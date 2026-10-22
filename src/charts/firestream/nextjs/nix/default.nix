# Next.js chart options aggregator
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (modules = [ ./nix/default.nix ]).
# It imports every per-section options module — each declaring
# `options.nextjs.<section>.*` with Model-A null defaults — and projects the
# resolved tree into the engine-declared `config.nextjs.values` attrset that the
# values.yaml emitter serialises.
#
# The Next.js chart has a FLAT shape (like odoo): the main Deployment's pod-spec
# lives at the top level of values.yaml, with a bundled `postgresql` subchart
# supplying the database (firestream-postgresql is injected by the flake-module).
#
# CRITICAL — `config` MUST be bound in the signature. `removeAttrs config.nextjs
# [ "_meta" "values" ]` strips those engine-declared keys BEFORE the recursive
# null-filter, otherwise `config.nextjs.values` would reference itself.
{ lib, config, ... }:

{
  imports = [
    ./options/common.nix
    ./options/global.nix
    ./options/image.nix
    ./options/app.nix
    ./options/service.nix
    ./options/ingress.nix
    ./options/service-account.nix
    ./options/network-policy.nix
    ./options/postgresql.nix
    ./options/external-database.nix
  ];

  # chart-manifest.json metadata. The bundled postgresql subchart must be Ready
  # before nextjs serves traffic, so --wait + a generous timeout cover image
  # pulls + subchart rollout. dependsOn stays empty: postgresql is bundled IN
  # this release, not a separate top-level stack dependency.
  config.nextjs._meta.deployment = lib.mapAttrs (_: lib.mkDefault) {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "10m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.nextjs._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # Image injection is supplied by the flake-module (nextjs main image +
  # postgresql subchart image); left empty here.
  config.nextjs._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`) into
  # the serialised `values` attrset, stripping null leaves recursively.
  config.nextjs.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.nextjs [ "_meta" "values" ]);
}
