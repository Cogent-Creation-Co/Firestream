# Cloudflared chart options: `image.*`.
#
# THE ONE PLACE THIS CHART DIFFERS FROM EVERY OTHER FIRESTREAM CHART.
#
# Elsewhere `image.*` is written by the flake-module's image-injection block
# from `firestreamImages.<app>` -- the Nix-built container that replaces
# Bitnami's. cloudflared has no such container: the connector is Cloudflare's
# own closed-source binary. So:
#
#   * there is no src/containers/firestream/cloudflared/;
#   * nix/flake-modules/charts/cloudflared.nix injects NOTHING at this path
#     (its containerRef is catalogue-only, componentPath = []);
#   * the chart's own values.yaml holds the pinned upstream triple
#     (docker.io / cloudflare/cloudflared / 2026.7.3) and is the source of
#     truth.
#
# This option therefore exists purely as a CONSUMER seam: a downstream Pulumi
# or flake can pin a different tag, a private mirror, or a digest, without
# forking the chart. Left null (the default) the chart's pin wins.
{ lib, chartTypes, ... }:

let
  t = chartTypes;
  inherit (lib) mkOption types;
in {
  options.cloudflared.image = mkOption {
    default = null;
    description = ''
      cloudflared image (registry/repository/tag/digest/pullPolicy/pullSecrets).

      UPSTREAM image, not a Firestream build. Set `tag` to move the pin, or
      `digest` for byte-exact reproducibility (digest wins over tag).
    '';
    type = types.nullOr t.imageType;
    example = {
      registry = "docker.io";
      repository = "cloudflare/cloudflared";
      tag = "2026.7.3";
    };
  };
}
