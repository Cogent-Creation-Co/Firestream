# Cloudflared chart options aggregator
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (modules = [ ./nix/default.nix ]).
# It imports every per-section options module -- each declaring
# `options.cloudflared.<section>.*` -- and projects the resolved tree into the
# engine-declared `config.cloudflared.values` attrset that the values.yaml
# emitter serialises.
#
# The cloudflared chart has a FLAT shape (like nginx/nextjs/odoo): the
# Deployment's pod-spec lives at the top level of values.yaml. It bundles NO
# subcharts beyond `common` -- the connector is a single stateless Deployment.
#
# One section is this chart's own, with no Bitnami analogue:
#   * `tunnel` (options/tunnel.nix) -- `existingSecret`,
#     `tunnelTokenSecretKey`, `metrics.enabled`, `extraArgs`. THE headline
#     surface: everything a deploying layer must set.
#
# Sections deliberately ABSENT (see values.yaml for the reasoning):
#   * `service`      -- cloudflared makes only OUTBOUND connections.
#   * persistence    -- the connector is stateless.
#   * ingress / TLS  -- both terminate at the Cloudflare edge, not here.
#
# ARCHITECTURAL NOTE (docs/firestream-supported-app.md §3): everything Nix emits
# here is a strict SUBSET of the chart's own value surface. Nix never
# synthesises chart templates or YAML manifests.
#
# CRITICAL -- `config` MUST be bound in the signature. `removeAttrs
# config.cloudflared [ "_meta" "values" ]` strips those engine-declared keys
# BEFORE the recursive null-filter, otherwise `config.cloudflared.values` would
# reference itself.
{ lib, config, ... }:

{
  imports = [
    ./options/common.nix
    ./options/global.nix
    ./options/image.nix
    ./options/app.nix
    ./options/service-account.nix

    # cloudflared-specific section
    ./options/tunnel.nix
  ];

  # chart-manifest.json metadata. A single stateless Deployment with no
  # subchart to wait on and no Jobs: a short timeout is right, and `wait` is
  # meaningful because readiness genuinely reflects edge registration (the
  # probes hit cloudflared's /ready).
  config.cloudflared._meta.deployment = {
    atomic = true;
    wait = true;
    waitForJobs = true;
    timeout = "5m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  # dependsOn stays empty ON PURPOSE. The connector's readiness depends only on
  # the Cloudflare edge accepting its token -- never on an in-cluster Service.
  # Its ingress rules are resolved by the edge at REQUEST time and point at
  # Service DNS names that need not exist when the connector starts (a request
  # arriving early gets a 502 from the edge, which is strictly better than a
  # namespace with no edge at all). So cloudflared may come up before, after,
  # or alongside nginx and the apps.
  config.cloudflared._meta.lifecycle = {
    dependsOn = [ ];
    lastBreakingVersion = null;
  };

  # Image injection is supplied by the flake-module
  # (nix/flake-modules/charts/cloudflared.nix). Unlike every other Firestream
  # chart it registers a CATALOGUE-ONLY entry (componentPath = []): cloudflared
  # is an upstream Cloudflare image, not a Nix-built one, so there is nothing
  # to substitute -- the chart's own values.yaml holds the pinned triple.
  config.cloudflared._meta.containerRefs = { };

  # Project the resolved option tree (minus the engine's `_meta`/`values`) into
  # the serialised `values` attrset, stripping null leaves recursively.
  config.cloudflared.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.cloudflared [ "_meta" "values" ]);
}
