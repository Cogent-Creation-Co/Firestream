# Nginx chart options: `serviceAccount.*`.
#
# Standard Bitnami ServiceAccount shape: create / name / annotations /
# automountServiceAccountToken.
{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.nginx.serviceAccount = mkOption {
    default = null;
    description = "ServiceAccount configuration for Nginx pods";
    type = types.nullOr (types.submodule {
      freeformType = types.attrsOf types.anything;

      options = {
        create = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Enable ServiceAccount creation";
        };

        name = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Name of the ServiceAccount to use";
        };

        annotations = mkOption {
          type = types.nullOr (types.attrsOf types.str);
          default = null;
          description = "Additional ServiceAccount annotations";
        };

        automountServiceAccountToken = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Automount the ServiceAccount token";
        };
      };
    });
  };
}
