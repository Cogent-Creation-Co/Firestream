# chart-manifest.json emitter (Phase 1 of the typed Nix->JSON->Rust contract)
# Copyright Firestream. MIT License.
#
# Serialises the chart's deployment-intent metadata into a JSON derivation.
# The shape it emits is the v1 schema every downstream consumer (Rust crate,
# aggregate index, etc.) reads from.
#
# Signature: { pkgs, lib }: cfg: <derivation producing chart-manifest.json>
#
# `cfg` is the resolved `config.${name}` attrset from eval-chart.nix's
# `lib.evalModules`. It is expected to carry:
#   cfg._meta.chartName       - Helm chart name
#   cfg._meta.releaseName     - Helm release name
#   cfg._meta.namespace       - target namespace
#   cfg._meta.deployment.*    - helm CLI knobs (atomic, wait, timeout, ...)
#   cfg._meta.lifecycle.*     - { dependsOn, lastBreakingVersion }
#   cfg._meta.containerRefs   - image map (Agent B fills in next phase)
#   cfg._meta.provenance.*    - { flakeRevision, nixpkgsRevision }
#
# Additional arguments come in via the partial-application call from
# eval-chart.nix:
#   chartVersion              - read from Chart.yaml
#   bundlePaths               - { chartPath; valuesPath; renderedPath; } using
#                               `builtins.placeholder "out"` so they resolve to
#                               the bundle derivation's actual store path.
#
# Mirrors the null-strip approach in ./to-values-yaml.nix: a `null` leaf in the
# manifest is the Nix idiom for "unset" - we drop those so the JSON stays a
# faithful contract. We deliberately retain explicit empty `""`, `[]`, `{}` as
# meaningful (e.g. `images = {}` => no overrides recorded yet, distinct from
# `images = null` => the option was never declared).
{ pkgs, lib }:

cfg:

{
  chartVersion,
  bundlePaths,
}:

let
  manifest = {
    schemaVersion = "1";
    name = cfg._meta.chartName;
    chart = cfg._meta.chartName;
    version = chartVersion;
    release = {
      namespace = cfg._meta.namespace;
      releaseName = cfg._meta.releaseName;
      # Same option that drives `--create-namespace` on the emitted bin/deploy
      # (eval-chart.nix). Both sinks read `cfg._meta.createNamespace` so the
      # shell path and the Rust path (helm_lifecycle/executor.rs) cannot
      # disagree about who owns namespace lifecycle.
      createNamespace = cfg._meta.createNamespace;
    };
    bundle = bundlePaths;
    deployment = {
      atomic = cfg._meta.deployment.atomic;
      wait = cfg._meta.deployment.wait;
      waitForJobs = cfg._meta.deployment.waitForJobs;
      timeout = cfg._meta.deployment.timeout;
      forceUpgrade = cfg._meta.deployment.forceUpgrade;
      hooksDisabled = cfg._meta.deployment.hooksDisabled;
      skipCrds = cfg._meta.deployment.skipCrds;
    };
    lifecycle = {
      dependsOn = cfg._meta.lifecycle.dependsOn;
      lastBreakingVersion = cfg._meta.lifecycle.lastBreakingVersion;
    };
    images = cfg._meta.containerRefs;
    provenance = {
      flakeRevision = cfg._meta.provenance.flakeRevision;
      nixpkgsRevision = cfg._meta.provenance.nixpkgsRevision;
    };
  };

  cleaned = lib.filterAttrsRecursive (_: v: v != null) manifest;
in
(pkgs.formats.json { }).generate "chart-manifest.json" cleaned
