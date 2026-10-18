# Cloudflared chart flake-module
# Copyright Firestream. MIT License.
#
# Wires the net-new cloudflared Helm chart through the options-driven evalChart
# entrypoint and contributes:
#   - packages.cloudflared-chart          (deployable chart bundle; builds on darwin)
#   - packages.cloudflared-base-chart     (un-overlaid chart, native defaults)
#   - firestreamCharts.cloudflared        (full evaluated chart result, for aggregate)
#   - firestreamChartImages.cloudflared   (consumer override API, for flake.lib.charts)
#
# Mirrors nix/flake-modules/charts/nginx.nix with ONE structural difference,
# and it is the interesting one:
#
#   THERE IS NO FIRESTREAM-BUILT CONTAINER FOR CLOUDFLARED.
#
# Every other chart's flake-module opens by re-evaluating
# `config.firestreamImages.<app>` to source an image triple, then injects it at
# the chart's image slot. cloudflared is Cloudflare's own closed-source
# connector binary: there is no src/containers/firestream/cloudflared/, no
# `packages.cloudflared`, and nothing for Nix to rebuild. Referencing
# `config.firestreamImages.cloudflared` would simply be an eval error.
#
# The image-injection machinery turns out to need no special-casing for this.
# `_meta.containerRefs` is a plain declarative attrset (eval-chart.nix), and
# inject-container-images.nix already defines `componentPath = []` as
# "catalogue-only: record in chart-manifest.json, contribute no values
# overlay". So we register the upstream triple as a catalogue entry: the
# manifest still documents exactly what image runs (which the deploy layer and
# any SBOM tooling reads), while the CHART'S OWN values.yaml stays the single
# source of truth for the pin. Nothing is injected, nothing is duplicated.
#
# Consequences worth knowing:
#   * `global.security.allowInsecureImages` is NOT set here. That flag exists
#     to get Firestream's non-Bitnami images past the `common` helpers'
#     whitelist; an upstream image needs no bypass.
#   * The generated values.yaml is EMPTY (`{}`) with no consumer overrides --
#     the strongest possible form of "the overlay is a strict subset of the
#     chart's value surface". `helm template -f` accepts it, and the
#     cloudflared-render-fidelity check asserts it stays a no-op.
#   * compose.nix iterates `firestreamImages`, so no docker-compose output is
#     produced for cloudflared. Correct: a tunnel connector has nothing to
#     stand up locally.
{ ... }: {
  perSystem = { pkgs, lib, config, evalChart, baseChart, ... }:
    let
      chartSrc = ../../../src/charts/firestream/cloudflared;
      optionsPath = chartSrc + "/nix/default.nix";

      # `common` only -- no postgresql, no redis. The connector is a single
      # stateless Deployment.
      subcharts = [
        { name = "common"; }
      ];

      # The upstream image, recorded for the manifest. MUST stay in sync with
      # src/charts/firestream/cloudflared/values.yaml, which is what actually
      # renders (componentPath = [] injects nothing).
      #
      # That "MUST" is ENFORCED, not merely requested: the
      # `cloudflared-render-fidelity` check (nix/flake-modules/charts/checks.nix)
      # reads the triple back out of chart-manifest.json and greps for it in the
      # chart's own rendered output, so a one-sided edit fails the build.
      upstreamImage = {
        registry = "docker.io";
        repository = "cloudflare/cloudflared";
        tag = "2026.7.3";
      };

      imageInjectionModule = { ... }: {
        config.cloudflared._meta.containerRefs = {
          cloudflared = {
            inherit (upstreamImage) registry repository tag;
            # CATALOGUE ONLY. Empty componentPath => recorded in
            # chart-manifest.json, no values overlay. See the header.
            componentPath = [ ];
          };
        };
      };

      c = evalChart {
        name = "cloudflared";
        inherit chartSrc subcharts;
        modules = [ optionsPath imageInjectionModule ];
      };
    in
    {
      packages.cloudflared-chart = c.chartBundle;

      packages.cloudflared-base-chart = baseChart {
        name = "cloudflared";
        inherit chartSrc subcharts;
      };

      firestreamCharts.cloudflared = c // { baseChart = config.packages.cloudflared-base-chart; };

      firestreamChartImages.cloudflared = {
        chartBundle = c.chartBundle;
        baseChart = config.packages.cloudflared-base-chart;
        render = c.render;
        eval = userMod: evalChart {
          name = "cloudflared";
          inherit chartSrc subcharts;
          modules = [ optionsPath imageInjectionModule userMod ];
        };
        options = c.options;
      };
    };
}
