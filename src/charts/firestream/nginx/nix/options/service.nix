# Nginx chart options: `service.*` (top-level Service for the Nginx pod).
#
# The Bitnami nginx chart defaults to `LoadBalancer` (port 80 -> targetPort
# http=8069). We type the most-overridden knobs and let freeformType
# passthrough handle the long tail.
{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.nginx.service = mkOption {
    default = null;
    description = "Service configuration for the Nginx pod";
    type = types.nullOr (types.submodule {
      freeformType = types.attrsOf types.anything;

      options = {
        type = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Nginx service type (LoadBalancer / ClusterIP / NodePort)";
        };

        ports = mkOption {
          type = types.nullOr (types.attrsOf (types.either types.str types.int));
          default = null;
          description = "Nginx service port map (e.g. { http = 80; })";
        };

        nodePorts = mkOption {
          type = types.nullOr (types.attrsOf (types.either types.str types.int));
          default = null;
          description = "NodePort assignments (use only with service.type=NodePort)";
        };

        sessionAffinity = mkOption {
          type = types.nullOr (types.enum [ "ClientIP" "None" ]);
          default = null;
          description = "Control where client requests go (same pod or round-robin)";
        };

        sessionAffinityConfig = mkOption {
          type = types.nullOr (types.attrsOf types.anything);
          default = null;
          description = "Additional settings for the sessionAffinity";
        };

        clusterIP = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Nginx service Cluster IP";
        };

        loadBalancerIP = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Nginx service Load Balancer IP";
        };

        loadBalancerSourceRanges = mkOption {
          type = types.nullOr (types.listOf types.str);
          default = null;
          description = "Nginx service Load Balancer sources";
        };

        externalTrafficPolicy = mkOption {
          type = types.nullOr (types.enum [ "Cluster" "Local" ]);
          default = null;
          description = "Nginx service external traffic policy";
        };

        annotations = mkOption {
          type = types.nullOr (types.attrsOf types.str);
          default = null;
          description = "Additional custom annotations for Nginx service";
        };

        extraPorts = mkOption {
          type = types.nullOr (types.listOf types.attrs);
          default = null;
          description = "Extra ports to expose on Nginx service";
        };
      };
    });
  };
}
