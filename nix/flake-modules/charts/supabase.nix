# Supabase chart flake-module.
# Copyright Firestream. MIT License.
#
# Wires the Supabase Helm chart (a Firestream fork of
# supabase-community/supabase-kubernetes, see src/charts/firestream/supabase/)
# through evalChart and contributes:
#   - packages.supabase-chart          (deployable chart bundle)
#   - packages.supabase-base-chart     (plain forked chart, native defaults)
#   - firestreamCharts.supabase        (full evaluated chart, for aggregate.nix)
#   - firestreamChartImages.supabase   (consumer override API, flake.lib.charts)
#
# Like seaweedfs, this is not a Bitnami chart: no allowInsecureImages guard,
# no path remaps, no subcharts. Each of the ten Supabase images has one
# `_meta.containerRefs` slot whose componentPath is its `image.<component>`
# block in values.yaml.
#
# The overlay also writes the development key set
# (firestreamLib.supabase.devSecrets) into secret.supabase.values. The chart's
# own values.yaml holds the same set, so checks.supabase-render-fidelity
# fails when the two drift.
{ ... }: {
  perSystem = { config, evalChart, baseChart, firestreamLib, ... }:
    let
      chartSrc = ../../../src/charts/firestream/supabase;
      optionsPath = chartSrc + "/nix/default.nix";
      subcharts = [ ];

      # chart component -> { registry key; evalContainer name }
      slots = {
        db = { key = "supabase-postgres"; container = "postgresql"; };
        auth = { key = "supabase-auth"; container = "gotrue"; };
        rest = { key = "supabase-postgrest"; container = "postgrest"; };
        realtime = { key = "supabase-realtime"; container = "realtime"; };
        supavisor = { key = "supabase-supavisor"; container = "supavisor"; };
        meta = { key = "supabase-postgres-meta"; container = "pgmeta"; };
        storage = { key = "supabase-storage"; container = "storage"; };
        imgproxy = { key = "supabase-imgproxy"; container = "imgproxy"; };
        studio = { key = "supabase-studio"; container = "studio"; };
        envoy = { key = "supabase-envoy"; container = "envoy"; };
      };

      # Reading the image options only forces option evaluation, not the
      # Linux-only image build, so this also evaluates on Darwin.
      imageRef = component: slot:
        let
          imgEval = config.firestreamImages.${slot.key}.eval (_: { });
          imgCfg = imgEval.config.${slot.container}.image;
        in
        {
          inherit (imgCfg) registry repository;
          tag = imgEval.imageTag;
          componentPath = [ "image" component ];
        };

      imageInjectionModule = { ... }: {
        config.supabase._meta.containerRefs = builtins.mapAttrs imageRef slots;
        config.supabase.secret.supabase.values = firestreamLib.supabase.devSecrets;
      };

      mkEval = extra: evalChart {
        name = "supabase";
        inherit chartSrc subcharts;
        modules = [ optionsPath imageInjectionModule ] ++ extra;
      };

      c = mkEval [ ];
    in
    {
      packages.supabase-chart = c.chartBundle;

      packages.supabase-base-chart = baseChart {
        name = "supabase";
        inherit chartSrc subcharts;
      };

      firestreamCharts.supabase = c // { baseChart = config.packages.supabase-base-chart; };

      firestreamChartImages.supabase = {
        chartBundle = c.chartBundle;
        baseChart = config.packages.supabase-base-chart;
        render = c.render;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
