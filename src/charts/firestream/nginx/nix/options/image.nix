# Nginx chart options: `image.*`.
#
# Single Bitnami-shaped image triple (registry/repository/tag/digest/
# pullPolicy/pullSecrets) plus a `debug` flag. The chart runs ONE image
# (firestream-nginx); there are no worker or init images.
#
# We use chartTypes.imageType (a submodule with
# `freeformType = types.attrsOf types.anything`) so extras like `debug`
# flow through unchanged.
{ lib, chartTypes, ... }:

let
  t = chartTypes;
  inherit (lib) mkOption types;
in {
  options.nginx.image = mkOption {
    default = null;
    description = "Nginx image (registry/repository/tag/digest/pullPolicy/pullSecrets/debug)";
    type = types.nullOr t.imageType;
  };
}
