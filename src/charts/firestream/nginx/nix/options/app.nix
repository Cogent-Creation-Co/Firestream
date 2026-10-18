# Nginx chart options: main nginx Deployment pod-spec (FLAT -- top-level keys).
#
# This is a NET-NEW Firestream chart, not a Bitnami fork, but it deliberately
# mirrors the flat Bitnami value shape used by nextjs/odoo: pod-spec knobs
# (replicaCount, containerPorts, probes, resources, security contexts,
# affinity/tolerations, autoscaling, pdb) live at the TOP LEVEL of values.yaml
# rather than nested under `app.*`. Keeping the shape uniform is what lets the
# generated values.yaml stay a faithful sparse override.
#
# Service / serviceAccount live in their own modules (separate top-level
# sections in values.yaml) -- they are NOT declared here. Neither are
# `upstreams` (options/upstreams.nix) or `proxy`/`nginxConfig`
# (options/proxy.nix), which are this chart's own additions.
#
# nginx is a stateless proxy, so there is deliberately NO persistence,
# ingress, or volumePermissions section.
{ lib, chartTypes, ... }:

let
  t = chartTypes;
  inherit (lib) mkOption types;
in {
  options.nginx = {
    # ----- Replicas / ports -----
    replicaCount = mkOption {
      type = types.nullOr types.int;
      default = null;
      description = "Number of Nginx replicas to deploy (ReadWriteMany PVC required if > 1)";
    };

    containerPorts = mkOption {
      type = types.nullOr (types.attrsOf (types.either types.str types.int));
      default = null;
      description = "Container ports for the Nginx pod (e.g. { http = 8069; })";
    };

    extraContainerPorts = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra container ports for the Nginx container(s)";
    };

    # ----- Resources / security contexts -----
    resourcesPreset = mkOption {
      type = t.resourcesPreset;
      default = null;
      description = "Resource preset (none/nano/micro/small/medium/large/xlarge/2xlarge)";
    };

    resources = mkOption {
      type = types.nullOr t.resourceRequirements;
      default = null;
      description = "Custom resource requirements (overrides resourcesPreset)";
    };

    podSecurityContext = mkOption {
      type = types.nullOr t.podSecurityContext;
      default = null;
      description = "Pod security context for the Nginx pod";
    };

    containerSecurityContext = mkOption {
      type = types.nullOr t.containerSecurityContext;
      default = null;
      description = "Container security context for the Nginx container";
    };

    # ----- Probes (extended set — nginx's readinessProbe carries a `path` -----
    # field for HTTP probing, not just the kubelet timing knobs).
    livenessProbe = mkOption {
      type = types.nullOr t.probeType;
      default = null;
      description = "Liveness probe configuration";
    };

    readinessProbe = mkOption {
      type = types.nullOr t.probeType;
      default = null;
      description = "Readiness probe configuration (carries a `path` field for HTTP probing)";
    };

    startupProbe = mkOption {
      type = types.nullOr t.probeType;
      default = null;
      description = "Startup probe configuration (carries a `path` field for HTTP probing)";
    };

    customLivenessProbe = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Custom liveness probe that overrides the default one";
    };

    customReadinessProbe = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Custom readiness probe that overrides the default one";
    };

    customStartupProbe = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Custom startup probe that overrides the default one";
    };

    # ----- Pod-level miscellany -----
    lifecycleHooks = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Container lifecycle hooks";
    };

    automountServiceAccountToken = mkOption {
      type = types.nullOr types.bool;
      default = null;
      description = "Mount Service Account token in pod";
    };

    hostAliases = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Pod host aliases";
    };

    podLabels = mkOption {
      type = types.nullOr (types.attrsOf types.str);
      default = null;
      description = "Extra labels for Nginx pods";
    };

    podAnnotations = mkOption {
      type = types.nullOr (types.attrsOf types.str);
      default = null;
      description = "Annotations for Nginx pods";
    };

    command = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      description = "Override default container command";
    };

    args = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      description = "Override default container args";
    };

    # ----- Scheduling -----
    podAffinityPreset = mkOption {
      type = types.nullOr (types.enum [ "" "soft" "hard" ]);
      default = null;
      description = "Pod affinity preset (ignored if `affinity` is set)";
    };

    podAntiAffinityPreset = mkOption {
      type = types.nullOr (types.enum [ "" "soft" "hard" ]);
      default = null;
      description = "Pod anti-affinity preset (ignored if `affinity` is set)";
    };

    nodeAffinityPreset = mkOption {
      type = types.nullOr t.nodeAffinityPreset;
      default = null;
      description = "Node affinity preset (ignored if `affinity` is set)";
    };

    affinity = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Affinity for pod assignment";
    };

    nodeSelector = mkOption {
      type = types.nullOr (types.attrsOf types.str);
      default = null;
      description = "Node labels for pod assignment";
    };

    tolerations = mkOption {
      type = types.nullOr (types.listOf t.tolerationType);
      default = null;
      description = "Tolerations for pod assignment";
    };

    topologySpreadConstraints = mkOption {
      type = types.nullOr (types.listOf t.topologySpreadConstraintType);
      default = null;
      description = "Topology spread constraints for pod assignment";
    };

    priorityClassName = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Nginx pods' Priority Class Name";
    };

    schedulerName = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Use an alternate scheduler (e.g. \"stork\")";
    };

    terminationGracePeriodSeconds = mkOption {
      type = types.nullOr (types.either types.str types.int);
      default = null;
      description = "Seconds Nginx pod needs to terminate gracefully";
    };

    updateStrategy = mkOption {
      type = types.nullOr t.updateStrategyType;
      default = null;
      description = "Nginx deployment update strategy";
    };

    # ----- Extra volumes / sidecars / init containers -----
    extraVolumes = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra volumes for Nginx pods";
    };

    extraVolumeMounts = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra volume mounts for Nginx container(s)";
    };

    sidecars = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Additional sidecar containers";
    };

    initContainers = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Additional init containers";
    };

    # ----- PDB / autoscaling -----
    pdb = mkOption {
      type = types.nullOr t.pdbType;
      default = null;
      description = "Pod Disruption Budget configuration";
    };

    autoscaling = mkOption {
      default = null;
      description = "Horizontal POD autoscaling configuration";
      type = types.nullOr (types.submodule {
        freeformType = types.attrsOf types.anything;
        options = {
          enabled = mkOption {
            type = types.nullOr types.bool;
            default = null;
            description = "Enable Horizontal POD autoscaling for Nginx";
          };
          minReplicas = mkOption {
            type = types.nullOr types.int;
            default = null;
            description = "Minimum number of Nginx replicas";
          };
          maxReplicas = mkOption {
            type = types.nullOr types.int;
            default = null;
            description = "Maximum number of Nginx replicas";
          };
          targetCPU = mkOption {
            type = types.nullOr (types.either types.str types.int);
            default = null;
            description = "Target CPU utilization percentage";
          };
          targetMemory = mkOption {
            type = types.nullOr (types.either types.str types.int);
            default = null;
            description = "Target Memory utilization percentage";
          };
        };
      });
    };
  };
}
