# Nginx chart flake-module
# Copyright Firestream. MIT License.
#
# Wires the net-new nginx Helm chart through the options-driven evalChart
# entrypoint and contributes:
#   - packages.nginx-chart           (deployable chart bundle; builds on darwin)
#   - packages.nginx-base-chart      (un-overlaid chart, native defaults)
#   - firestreamCharts.nginx         (full evaluated chart result, for aggregate)
#   - firestreamChartImages.nginx    (consumer override API, for flake.lib.charts)
#
# Mirrors nix/flake-modules/charts/nextjs.nix, minus the database: nginx is the
# per-namespace reverse proxy and bundles no backing services, so `common` is
# the only vendored subchart and there is exactly ONE image to inject.
{ ... }: {
  perSystem = { pkgs, lib, config, evalChart, baseChart, ... }:
    let
      chartSrc = ../../../src/charts/firestream/nginx;
      optionsPath = chartSrc + "/nix/default.nix";

      # `common` only -- no postgresql, no redis. Vendored from the in-repo
      # Bitnami fork by the engine's vendor-subcharts.nix.
      subcharts = [
        { name = "common"; }
      ];

      # nginx container image triple (re-eval without overrides). `.eval` reads
      # only config/imageTag -- safe on darwin where dockerImage is gated.
      nginxImg =
        let
          imgEval = config.firestreamImages.nginx.eval (_: { });
          imgCfg = imgEval.config.nginx.image;
        in
        {
          registry = imgCfg.registry;
          repository = imgCfg.repository;
          tag = imgEval.imageTag;
        };

      imageInjectionModule = { ... }: {
        config.nginx._meta.containerRefs = {
          nginx = {
            inherit (nginxImg) registry repository tag;
            # Flat chart shape: the single image lives at top-level `image:`.
            componentPath = [ "image" ];
          };
        };
        # firestream-nginx is not in the Bitnami image whitelist the `common`
        # helpers enforce; bypass it as every other Firestream chart does.
        config.nginx.global.security.allowInsecureImages = true;
      };

      c = evalChart {
        name = "nginx";
        inherit chartSrc subcharts;
        modules = [ optionsPath imageInjectionModule ];
      };
    in
    {
      packages.nginx-chart = c.chartBundle;

      packages.nginx-base-chart = baseChart {
        name = "nginx";
        inherit chartSrc subcharts;
      };

      firestreamCharts.nginx = c // { baseChart = config.packages.nginx-base-chart; };

      firestreamChartImages.nginx = {
        chartBundle = c.chartBundle;
        baseChart = config.packages.nginx-base-chart;
        render = c.render;
        eval = userMod: evalChart {
          name = "nginx";
          inherit chartSrc subcharts;
          modules = [ optionsPath imageInjectionModule userMod ];
        };
        options = c.options;
      };
    };
}
