# Cloudflared chart options: common / top-level shared parameters.
#
# Covers values.yaml keys at the very top level (outside named sections):
# kubeVersion, nameOverride, fullnameOverride, namespaceOverride, commonLabels,
# commonAnnotations, clusterDomain, extraDeploy, diagnosticMode.
#
# Model A: every leaf is nullOr + default = null (sparse override), so an unset
# knob is stripped from the emitted values.yaml and the chart's own default
# wins.
{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.cloudflared = {
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
      description = ''
        String to fully override common.names.fullname. Also renames the
        default tunnel-token Secret, which is `<fullname>-token` whenever
        `existingSecret` is unset.
      '';
    };

    namespaceOverride = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = ''
        Namespace the rendered objects are placed in, overriding
        `.Release.Namespace`. Prefer `_meta.namespace`, which moves the Helm
        release itself (and therefore the generated deploy script) rather than
        just the manifest metadata.
      '';
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

    diagnosticMode = mkOption {
      default = null;
      description = "Diagnostic mode configuration (all probes disabled, container command overridden)";
      type = types.nullOr (types.submodule {
        freeformType = types.attrsOf types.anything;
        options = {
          enabled = mkOption {
            type = types.nullOr types.bool;
            default = null;
            description = "Enable diagnostic mode";
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
