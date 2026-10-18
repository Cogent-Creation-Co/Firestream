# Airflow chart options aggregator (Phase 4)
# Copyright Firestream. MIT License.
#
# Consumed as a NixOS module by `evalChart` (passed via `modules = [ ./nix/default.nix ]`).
# It imports all 25 option modules — each declaring `options.airflow.<section>.*`
# (Phase 3) — and projects the resolved option tree into the engine-declared
# `config.airflow.values` attrset that the values.yaml emitter serialises.
#
# CRITICAL — the `config` argument MUST be bound in the module signature below.
# The legacy `nix/modules/default.nix` was broken because it referenced `config`
# without binding it. We bind `config` here and read it back. Equally important:
# `builtins.removeAttrs config.airflow [ "_meta" "values" ]` strips those two keys
# BEFORE the recursive filter runs. `_meta`/`values` are declared by the engine's
# stdSchema, so leaving `values` in the tree would make `config.airflow.values`
# reference itself (infinite recursion). We read every OTHER section, drop nulls
# recursively, and write the result back to `values`.
{ lib, config, ... }:

{
  imports = [
    # Core Airflow configuration
    ./options/global.nix
    ./options/common.nix
    ./options/airflow-core.nix
    ./options/image.nix
    ./options/auth.nix
    ./options/dags.nix
    ./options/plugins.nix
    ./options/init-containers.nix
    ./options/sidecars.nix

    # Airflow components
    ./options/web.nix
    ./options/scheduler.nix
    ./options/dag-processor.nix
    ./options/triggerer.nix
    ./options/worker.nix
    ./options/setup-db-job.nix

    # Networking and exposure
    ./options/service.nix
    ./options/ingress.nix
    ./options/metrics.nix

    # Security and access
    ./options/rbac.nix
    ./options/ldap.nix
    ./options/service-account.nix

    # Subcharts and external services
    ./options/postgresql.nix
    ./options/redis.nix
    ./options/external-database.nix
    ./options/external-redis.nix
  ];

  # ------------------------------------------------------------------
  # Phase 1 — chart-manifest.json metadata (canary).
  #
  # These _meta knobs surface in `$out/chart-manifest.json` (built by the
  # engine via lib/to-chart-manifest.nix) and lock the v1 JSON contract
  # consumed by downstream agents (B: image injection; C: aggregate index;
  # D: Rust deploy layer).
  #
  # - deployment: helm install/upgrade flags. `wait`/`waitForJobs`/`atomic` are
  #   all OFF, and that is load-bearing — see the deadlock note below.
  # - lifecycle.dependsOn: postgres + redis subcharts must converge first
  #   when they're externalised; harmless on the bundled deployment.
  # - lifecycle.lastBreakingVersion: no airflow handler exists in
  #   src/lib/rust/firestream/src/deploy/helm_lifecycle/charts/ (only
  #   prometheus/nginx/postgresql/kafka/external-dns), so we leave it null.
  # - containerRefs: Agent B (next phase) populates this from the container
  #   registry; kept as an empty attrset here so the manifest schema stays
  #   valid (the engine's null-strip would otherwise drop the field).
  # - provenance: framework.nix does not yet pass `inputs.self.rev` into
  #   evalChart, so both fields remain null. See the TODO in
  #   bin/nix/firestream/charts/eval-chart.nix.
  # ------------------------------------------------------------------
  # `--wait` DEADLOCKS this chart. Every airflow pod (web/scheduler/worker/
  # triggerer/dag-processor) carries a `wait-for-db-migrations` init container
  # that blocks until the Airflow DB is migrated, and the migration Job
  # (templates/setup-db-job.yaml) is annotated
  # `helm.sh/hook: post-install,post-upgrade`. Helm runs post-install hooks
  # only AFTER the release's own resources are ready, so with `--wait`:
  #
  #     pods wait for migrations -> migrations are a post-install hook
  #       -> hook waits for pods to be ready -> circular wait
  #
  # It then burns the entire timeout and, with `--atomic`, uninstalls the
  # release — so the failure surfaces as a bare `context deadline exceeded`
  # with no pods, events or logs left to diagnose. `--atomic` cannot be kept
  # on its own either: helm sets `--wait` automatically whenever `--atomic`
  # is used, so the two must go together.
  #
  # Measured on a k3d cluster with all three images preloaded:
  #   --wait --wait-for-jobs --atomic --timeout 20m  -> FAILS at 20:00.63
  #   (no wait flags)                                -> all 7 pods 1/1 in ~3m,
  #                                                     /api/v2/monitor/health 200
  #
  # Readiness is still gated, just by the caller rather than by helm: the k8s
  # e2e harness performs its own pod-Ready wait plus a per-protocol probe
  # (see src/lib/rust/firestream-e2e-k8s), which is the layer that can observe
  # the migration hook completing.
  config.airflow._meta.deployment = {
    atomic = false;
    wait = false;
    waitForJobs = false;
    timeout = "20m";
    forceUpgrade = false;
    hooksDisabled = false;
    skipCrds = false;
  };

  config.airflow._meta.lifecycle = {
    dependsOn = [ "postgresql" "redis" ];
    lastBreakingVersion = null;
  };

  # Project the resolved option tree (minus the engine's `_meta`/`values`) into
  # the serialised `values` attrset, stripping null leaves recursively. The
  # emitter (to-values-yaml.nix) does its own null-strip too, but filtering here
  # keeps `config.airflow.values` a faithful, null-free mirror of the overrides.
  config.airflow.values =
    lib.filterAttrsRecursive (_: v: v != null)
      (builtins.removeAttrs config.airflow [ "_meta" "values" ]);
}
