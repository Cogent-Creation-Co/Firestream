# Chart aggregate flake-module (Phase 3)
# Copyright Firestream. MIT License.
#
# The chart-side mirror of nix/flake-modules/aggregate.nix. Collects every chart
# registered in `config.firestreamCharts` into a single symlink-farm derivation
# at `packages.firestream-charts-bundle` and emits an `index.json` at the root
# that catalogues both per-chart manifest paths and named stacks.
#
# The Rust deploy layer (Agent D, next phase) mounts this derivation's $out as
# its `/opt/firestream/charts/` default via a clap fallback. It then reads
# `index.json` to discover available charts/stacks and resolves each chart's
# `chart-manifest.json` by joining the manifest path against the farm root.
#
# DESIGN:
#   - The registry option `config.firestreamCharts` is `lazyAttrsOf raw`, so it
#     auto-expands as Wave-3 chart flake-modules land (postgresql/redis/kafka/
#     spark/jupyterhub/superset/odoo). Phase 3 only ships airflow; the others
#     simply aren't in the attrset yet, and the loop here picks up whatever is
#     present at evaluation time.
#   - Stacks may reference charts that aren't yet registered. The `dev` stack
#     declared below lists the full target stack; charts that have no entry in
#     `firestreamCharts` are recorded in `index.json` under `stacks` but
#     receive no symlink. The Rust reader handles missing charts at the
#     resolution layer, not here.
#   - We filter `firestreamCharts` to entries that actually carry a
#     `chartBundle` attribute before iterating, so a partially-registered
#     entry (no chartBundle yet) is skipped gracefully rather than crashing
#     the symlink farm.
#   - `index.json` paths are RELATIVE to the farm root (`$out`). That keeps
#     them stable across mount points (the Rust runtime may bind-mount the
#     farm anywhere). The reader resolves via `farm_root.join(manifest_path)`.
#
# Like nix/flake-modules/charts/checks.nix, this module does NOT gate on
# `config.firestreamCharts ? <n>` at the option-emission level (that would be
# self-referential and infinitely recur through flake-parts' option-merge).
# All references to the registry happen lazily inside the runCommand.
{ ... }: {
  perSystem = { config, pkgs, lib, ... }:
    let
      # Filter to charts that have a `chartBundle` derivation attached. This
      # skips placeholder/in-progress registrations gracefully.
      chartsWithBundle = lib.filterAttrs
        (_: v: lib.isAttrs v && v ? chartBundle)
        config.firestreamCharts;

      chartNames = lib.attrNames chartsWithBundle;

      # Filter to charts that ALSO carry a `baseChart` derivation (the
      # un-overlaid, native-defaults chart surfaced in Phase 5). Defensive:
      # only chart entries that actually have the attr are included.
      chartsWithBase = lib.filterAttrs
        (_: v: lib.isAttrs v && v ? baseChart)
        config.firestreamCharts;

      baseChartNames = lib.attrNames chartsWithBase;

      # index.json schema (v1):
      #   {
      #     "schemaVersion": "1",
      #     "charts":  { "<name>": { "manifestPath": "<name>/chart-manifest.json" } },
      #     "stacks":  { "<stack>": [ "<chartName>", ... ] }
      #   }
      # `manifestPath` is RELATIVE to the farm root so the Rust reader can
      # resolve via `charts_dir.join(manifest_path)` regardless of mount point.
      indexValue = {
        schemaVersion = "1";
        charts = lib.listToAttrs (map
          (n: {
            name = n;
            value = { manifestPath = "${n}/chart-manifest.json"; };
          })
          chartNames);
        # Base (un-overlaid) charts. Each `<name>-base` symlink points at the
        # chart's own native-defaults derivation; `baseChartPath` is RELATIVE
        # to the farm root, like `manifestPath` above.
        baseCharts = lib.listToAttrs (map
          (n: {
            name = n;
            value = { baseChartPath = "${n}-base"; };
          })
          baseChartNames);
        stacks = config.firestreamStacks;
      };

      indexJsonDrv = (pkgs.formats.json { }).generate "firestream-charts-index.json" indexValue;

      # The symlink farm itself. For each registered chart we create
      # `$out/<chart>` pointing at the chart's `chartBundle` derivation, then
      # drop `index.json` at the root. We use a plain `runCommand` (not
      # `symlinkJoin`) because we want each chart at a top-level <name>/
      # directory rather than the merged-leaves layout symlinkJoin produces.
      symlinkLines = lib.concatMapStringsSep "\n"
        (n: "ln -s ${chartsWithBundle.${n}.chartBundle} \"$out/${n}\"")
        chartNames;

      # Per-base-chart symlinks: $out/<name>-base -> <baseChart store path>.
      baseSymlinkLines = lib.concatMapStringsSep "\n"
        (n: "ln -s ${chartsWithBase.${n}.baseChart} \"$out/${n}-base\"")
        baseChartNames;
    in
    {
      # Stack declarations. The `dev` stack lists the full local data-platform
      # target order (charts deploy in this sequence). Phase 3 only resolves
      # `airflow`; the other names are placeholders for Wave-3 charts.
      firestreamStacks.dev = [
        "seaweedfs"
        "postgresql"
        "redis"
        "airflow"
        "kafka"
        "spark"
        "jupyterhub"
        "superset"
        "odoo"
        "nextjs"
        # nginx LAST, deliberately. It is the namespace's edge proxy, so it has
        # no data-plane dependants to deploy before -- but with the default
        # (empty) `nginxConfig.resolver` nginx resolves each `proxy_pass` name
        # at CONFIG LOAD and refuses to start with "host not found in upstream"
        # if a backend Service is missing. Deploying it after the apps it fronts
        # guarantees those Services exist. (Set `nginxConfig.resolver` to the
        # cluster DNS address to lift that constraint; the ordering then only
        # affects how quickly the edge becomes useful, not whether it starts.)
        "nginx"
      ];

      # The edge stack. SEPARATE FROM `dev` ON PURPOSE, and the reason is
      # cloudflared: its TUNNEL_TOKEN secretKeyRef is deliberately not
      # `optional`, and it deploys with `atomic`/`wait`/5m
      # (src/charts/firestream/cloudflared/nix/default.nix). On a local cluster,
      # where nothing has provisioned a Cloudflare tunnel, the pods sit in
      # CreateContainerConfigError until helm gives up and rolls back -- which
      # would take the whole `dev` deploy down with it. A local data platform
      # has no business dialling out to the Cloudflare edge, so `dev` no longer
      # lists it.
      #
      # Order within the stack: nginx first so the connector's in-cluster target
      # exists before the tunnel goes live (it would otherwise briefly answer
      # 502 -- the edge resolves ingress rules at REQUEST time, so cloudflared
      # itself starts happily with nothing behind it).
      firestreamStacks.edge = [
        "nginx"
        "cloudflared"
      ];

      packages.firestream-charts-bundle = pkgs.runCommand "firestream-charts-bundle"
        {
          # Surface the raw index value as passthru for debugging / Nix-side
          # consumers that want to introspect the catalogue without IFD.
          passthru = {
            index = indexValue;
            chartNames = chartNames;
            stacks = config.firestreamStacks;
          };
        } ''
        set -euo pipefail
        mkdir -p "$out"

        # Per-chart symlinks: $out/<name> -> <chartBundle store path>
        ${symlinkLines}

        # Per-base-chart symlinks: $out/<name>-base -> <baseChart store path>
        ${baseSymlinkLines}

        # Top-level index.json catalogues charts + stacks.
        cp ${indexJsonDrv} "$out/index.json"
      '';
    };
}
