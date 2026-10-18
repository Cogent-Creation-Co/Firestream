# Cloudflared chart options: `global.*`.
#
# Model A: nullOr leaves with freeformType passthrough. There are no subcharts
# beyond `common`, so the only globals that matter in practice are
# imageRegistry / imagePullSecrets (for pulling cloudflare/cloudflared through
# a mirror or private registry).
{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.cloudflared.global = mkOption {
    default = null;
    description = "Global Docker image parameters and cross-chart settings";
    type = types.nullOr (types.submodule {
      freeformType = types.attrsOf types.anything;

      options = {
        imageRegistry = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Global Docker image registry (e.g. a pull-through mirror for docker.io)";
        };

        imagePullSecrets = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          description = "Global Docker registry secret names as an array";
        };

        defaultStorageClass = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Global default StorageClass (unused: cloudflared is stateless)";
        };

        security = mkOption {
          default = null;
          description = "Global security settings";
          type = types.nullOr (types.submodule {
            freeformType = types.attrsOf types.anything;
            options = {
              allowInsecureImages = mkOption {
                type = types.nullOr types.bool;
                default = null;
                description = ''
                  Allows skipping image verification. Unlike the other
                  Firestream charts this stays UNSET here: cloudflared runs
                  Cloudflare's own upstream image, not a Firestream-built one,
                  so there is no whitelist to bypass.
                '';
              };
            };
          });
        };

        compatibility = mkOption {
          default = null;
          description = "Compatibility adaptations for Kubernetes platforms";
          type = types.nullOr (types.submodule {
            freeformType = types.attrsOf types.anything;
            options = {
              openshift = mkOption {
                default = null;
                description = "Adaptations for OpenShift";
                type = types.nullOr (types.submodule {
                  freeformType = types.attrsOf types.anything;
                  options = {
                    adaptSecurityContext = mkOption {
                      type = types.nullOr (types.enum [ "auto" "force" "disabled" ]);
                      default = null;
                      description = "Adapt the securityContext sections of the deployment to make them work on OpenShift";
                    };
                  };
                });
              };
            };
          });
        };
      };
    });
  };
}
