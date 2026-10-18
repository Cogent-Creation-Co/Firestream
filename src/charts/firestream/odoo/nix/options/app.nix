# Odoo chart options: main odoo Deployment pod-spec (FLAT — top-level keys).
#
# Bitnami odoo's Deployment configuration is FLAT in values.yaml: pod-spec
# knobs (replicaCount, containerPorts, probes, resources, security
# contexts, affinity/tolerations, autoscaling, pdb) all live at the
# top level rather than nested under `app.*` or `web.*`. We mirror
# the upstream shape exactly so the generated values.yaml is a
# faithful sparse override and the render-fidelity check stays a
# no-op.
#
# Service / ingress / persistence / networkPolicy / volumePermissions /
# serviceAccount / pdb are split into their own modules (separate
# top-level sections in values.yaml) — they are NOT declared here.
{ lib, chartTypes, ... }:

let
  t = chartTypes;
  inherit (lib) mkOption types;
in {
  options.odoo = {
    # ----- Replicas / ports -----
    replicaCount = mkOption {
      type = types.nullOr types.int;
      default = null;
      description = "Number of Odoo replicas to deploy (ReadWriteMany PVC required if > 1)";
    };

    workers = mkOption {
      type = types.nullOr types.ints.unsigned;
      default = null;
      example = 2;
      description = ''
        Number of Odoo HTTP worker processes (`workers` in odoo.conf), emitted
        as the ODOO_WORKERS env var (chart default 0).

        THE gevent-port switch, and the single setting that controls it. `0`
        runs Odoo threaded: one process bound to `containerPorts.http` only,
        serving websockets on that port, with **`containerPorts.gevent` (8072)
        NOT bound by anything**. Any value above 0 switches Odoo to prefork,
        which spawns the gevent worker that binds 8072.

        Because the container port, the Service port and the env var are all
        gated on this one value, they cannot drift apart. Set it above 0
        whenever something expects 8072 to answer -- typically a reverse proxy
        routing `/websocket` and `/longpolling`, which is exactly what the
        firestream nginx chart's `upstreams.<name>.routes` generates.

        Note this is a per-DEPLOYMENT choice, not an image choice: the same
        container also has `config.odoo.workers`
        (src/containers/firestream/odoo/options.nix) for the docker-compose
        loop, and the chart's env wins over the image's baked default.
      '';
    };

    containerPorts = mkOption {
      type = types.nullOr (types.attrsOf (types.either types.str types.int));
      default = null;
      description = ''
        Container ports for the Odoo pod (e.g. { http = 8069; gevent = 8072; }).

        `gevent` is only declared on the pod when `workers` > 0 -- see there.
      '';
    };

    extraContainerPorts = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra container ports for the Odoo container(s)";
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
      description = "Pod security context for the Odoo pod";
    };

    containerSecurityContext = mkOption {
      type = types.nullOr t.containerSecurityContext;
      default = null;
      description = "Container security context for the Odoo container";
    };

    # ----- Probes (extended set — odoo's readinessProbe carries a `path` -----
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
      description = "Extra labels for Odoo pods";
    };

    podAnnotations = mkOption {
      type = types.nullOr (types.attrsOf types.str);
      default = null;
      description = "Annotations for Odoo pods";
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
      description = "Odoo pods' Priority Class Name";
    };

    schedulerName = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Use an alternate scheduler (e.g. \"stork\")";
    };

    terminationGracePeriodSeconds = mkOption {
      type = types.nullOr (types.either types.str types.int);
      default = null;
      description = "Seconds Odoo pod needs to terminate gracefully";
    };

    updateStrategy = mkOption {
      type = types.nullOr t.updateStrategyType;
      default = null;
      description = "Odoo deployment update strategy";
    };

    # ----- Extra volumes / sidecars / init containers -----
    extraVolumes = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra volumes for Odoo pods";
    };

    extraVolumeMounts = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra volume mounts for Odoo container(s)";
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
            description = "Enable Horizontal POD autoscaling for Odoo";
          };
          minReplicas = mkOption {
            type = types.nullOr types.int;
            default = null;
            description = "Minimum number of Odoo replicas";
          };
          maxReplicas = mkOption {
            type = types.nullOr types.int;
            default = null;
            description = "Maximum number of Odoo replicas";
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
