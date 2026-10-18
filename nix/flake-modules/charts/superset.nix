# Superset chart flake-module (Phase 6b - Agent G6; Phase B image injection)
# Copyright Firestream. MIT License.
#
# Wires the Superset Helm chart through the options-driven evalChart
# entrypoint and contributes:
#   - packages.superset-chart            (deployable chart bundle; builds on darwin)
#   - firestreamCharts.superset          (full evaluated chart result, for aggregate)
#   - firestreamChartImages.superset     (consumer override API, for flake.lib.charts)
#
# Mirrors nix/flake-modules/charts/jupyterhub.nix. No isLinux gate —
# `helm template` runs in the build sandbox on every platform.
#
# The Bitnami Superset chart bundles `common` + `postgresql` + `redis` as
# vendored subcharts (Chart.yaml v4.0.1 lists all three). vendor-subcharts.nix
# consumes the in-repo Bitnami fork at src/charts/bitnami/. The recursive
# vendoring handles postgresql's own `common` dependency.
#
# Phase B image injection: Bitnami Superset ships ONE main image
# (bitnami/superset — shared across web/worker/beat/flower/init via the chart's
# `image` template helper), plus the bundled postgresql and redis subchart
# images. We wire all three with firestream-* containers.
# `global.security.allowInsecureImages` is flipped to bypass Bitnami's
# NOTES.txt whitelist.
{ ... }: {
  perSystem = { pkgs, lib, config, evalChart, baseChart, ... }:
    let
      chartSrc = ../../../src/charts/firestream/superset;
      optionsPath = chartSrc + "/nix/default.nix";

      # Subcharts vendored from the in-repo Bitnami fork. Superset's
      # Chart.yaml lists `redis` (21.x.x), `postgresql` (16.x.x), and
      # `common` (2.x.x). The engine's vendor-subcharts.nix handles the
      # nested copy AND postgresql's own `common` dependency.
      subcharts = [
        { name = "common"; }
        { name = "postgresql"; }
        { name = "redis"; }
      ];

      supersetImg =
        let
          imgEval = config.firestreamImages.superset.eval (_: {});
          imgCfg = imgEval.config.superset.image;
        in {
          registry = imgCfg.registry;
          repository = imgCfg.repository;
          tag = imgEval.imageTag;
        };

      pgImg =
        let
          imgEval = config.firestreamImages.postgresql.eval (_: {});
          imgCfg = imgEval.config.postgresql.image;
        in {
          registry = imgCfg.registry;
          repository = imgCfg.repository;
          tag = imgEval.imageTag;
        };

      redisImg =
        let
          imgEval = config.firestreamImages.redis.eval (_: {});
          imgCfg = imgEval.config.redis.image;
        in {
          registry = imgCfg.registry;
          repository = imgCfg.repository;
          tag = imgEval.imageTag;
        };

      # Phase 3 path-de-branding: firestream-superset now bakes its SUPERSET_*
      # dirs under /opt/firestream/superset, co-locating config + logs under the
      # superset_home emptyDir (writable) and TMP at /tmp — see
      # src/containers/firestream/superset/5/options.nix. The chart templates
      # mount the superset_home emptyDir at /opt/firestream/superset/superset_home
      # (matching the container), so the previous chart-side path-override
      # extraEnvVars are no longer needed and have been removed.

      # Phase 1 path-de-branding: the postgresql subchart templates + values now
      # mount/reference firestream paths directly, matching the firestream-postgresql
      # container's baked *_DIR vars. No subchart pg path-override extraEnvVars needed.
      imageInjectionModule = { ... }: {
        config.superset._meta.containerRefs = {
          superset = {
            inherit (supersetImg) registry repository tag;
            componentPath = [ "image" ];
          };
          postgresql = {
            inherit (pgImg) registry repository tag;
            componentPath = [ "postgresql" "image" ];
          };
          redis = {
            inherit (redisImg) registry repository tag;
            componentPath = [ "redis" "image" ];
          };
        };
        config.superset.global.security.allowInsecureImages = true;
        # The bundled Bitnami postgresql subchart defaults to preloading the
        # `pgaudit` extension (postgresqlSharedPreloadLibraries: "pgaudit"), which
        # the firestream-postgresql image does not ship, causing the server start
        # to fail with `could not access file "pgaudit"`. Mirror the standalone
        # postgresql overlay and clear it for the subchart.
        config.superset.postgresql.postgresqlSharedPreloadLibraries = "";
      };

      # `loadExamples` is only half-wired upstream, and the missing half OOM-kills
      # the release.
      #
      # The chart threads `.Values.loadExamples` into the init Job only
      # (templates/init/init-job.yaml sets SUPERSET_LOAD_EXAMPLES there). The
      # web / worker / beat / flower pods never receive it. Meanwhile the
      # firestream-superset container BAKES `SUPERSET_LOAD_EXAMPLES = "true"`
      # as its own default (src/containers/firestream/superset/5/options.nix:99),
      # and env-defaults semantics are `export VAR="''${VAR:-default}"` — so with
      # nothing injected, the container default wins and EVERY pod tries to load
      # the example dashboards at startup.
      #
      # Observed on k3d: the web pod reaches
      #   `superset.cli.examples:Loading [World Bank's Health Nutrition and
      #    Population Stats]`
      # then exits 137 OOMKilled against the chart's `web.resourcesPreset =
      # "small"` (768Mi limit), CrashLoopBackOffs, never goes Ready, and helm
      # burns its 10m timeout before `--atomic` rolls the release back — leaving
      # only `context deadline exceeded` to debug from.
      #
      # Injecting the var explicitly makes the pods agree with the chart's own
      # `loadExamples` setting instead of silently disagreeing with it. This is
      # the standard Bitnami-compat `extraEnvVars` seam (see CLAUDE.md,
      # "Path remap"), applied per-component because this chart is hub-and-spoke.
      loadExamplesEnv = [
        { name = "SUPERSET_LOAD_EXAMPLES"; value = "false"; }
      ];

      loadExamplesModule = { ... }: {
        config.superset.web.extraEnvVars = loadExamplesEnv;
        config.superset.worker.extraEnvVars = loadExamplesEnv;
        config.superset.beat.extraEnvVars = loadExamplesEnv;
        config.superset.flower.extraEnvVars = loadExamplesEnv;
      };

      c = evalChart {
        name = "superset";
        inherit chartSrc subcharts;
        modules = [ optionsPath imageInjectionModule loadExamplesModule ];
      };
    in
    {
      packages.superset-chart = c.chartBundle;

      # Base (un-overlaid) chart: chart's OWN native defaults, no Firestream
      # values overlay / no image injection. Renders `bitnami/superset`.
      packages.superset-base-chart = baseChart {
        name = "superset";
        inherit chartSrc subcharts;
      };

      # Registry: full evaluated chart result (used by aggregate.nix / flake.lib).
      firestreamCharts.superset = c // { baseChart = config.packages.superset-base-chart; };

      # Registry: consumer override API exposed via flake.lib.<sys>.charts.superset.
      firestreamChartImages.superset = {
        chartBundle = c.chartBundle;
        baseChart = config.packages.superset-base-chart;
        render = c.render;
        eval = userMod: evalChart {
          name = "superset";
          inherit chartSrc subcharts;
          modules = [ optionsPath imageInjectionModule userMod ];
        };
        options = c.options;
      };
    };
}
