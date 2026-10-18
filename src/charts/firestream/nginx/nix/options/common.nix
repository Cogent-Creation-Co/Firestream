# Nginx chart options: common / top-level shared parameters.
#
# Covers values.yaml keys at the very top level (outside named sections):
# kubeVersion, nameOverride, fullnameOverride, commonLabels, commonAnnotations,
# clusterDomain, extraDeploy, usePasswordFiles, diagnosticMode.
#
# Model A: every leaf is nullOr + default = null (sparse override).
{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.nginx = {
    kubeVersion = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Override Kubernetes version";
    };

    nameOverride = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "String to partially override common.names.fullname";
    };

    fullnameOverride = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "String to fully override common.names.fullname";
    };

    commonLabels = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Labels to add to all deployed objects";
    };

    commonAnnotations = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Annotations to add to all deployed objects";
    };

    clusterDomain = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Default Kubernetes cluster domain";
    };

    extraDeploy = mkOption {
      type = types.nullOr (types.listOf types.anything);
      default = null;
      description = "Array of extra objects to deploy with the release";
    };

    usePasswordFiles = mkOption {
      type = types.nullOr types.bool;
      default = null;
      description = "Mount credentials as files instead of using environment variables";
    };

    diagnosticMode = mkOption {
      default = null;
      description = "Diagnostic mode configuration";
      type = types.nullOr (types.submodule {
        freeformType = types.attrsOf types.anything;
        options = {
          enabled = mkOption {
            type = types.nullOr types.bool;
            default = null;
            description = "Enable diagnostic mode (all probes disabled, container command overridden)";
          };

          command = mkOption {
            type = types.nullOr (types.listOf types.str);
            default = null;
            description = "Command to override all containers in the deployment";
          };

          args = mkOption {
            type = types.nullOr (types.listOf types.str);
            default = null;
            description = "Args to override all containers in the deployment";
          };
        };
      });
    };
  };
}
