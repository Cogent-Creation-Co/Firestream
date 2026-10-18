# Cloudflared chart options: `serviceAccount.*`.
#
# Standard Bitnami ServiceAccount shape: create / name / annotations /
# automountServiceAccountToken.
#
# The connector never talks to the Kubernetes API, so the token stays
# unmounted; the ServiceAccount exists to give the pod a non-`default` identity
# and a stable object for a deploying layer to annotate (e.g. GKE Workload
# Identity).
{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.cloudflared.serviceAccount = mkOption {
    default = null;
    description = "ServiceAccount configuration for the cloudflared pods";
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
          description = "Additional ServiceAccount annotations (e.g. iam.gke.io/gcp-service-account)";
        };

        automountServiceAccountToken = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Automount the ServiceAccount token (chart default false - the connector needs no API access)";
        };
      };
    });
  };
}
