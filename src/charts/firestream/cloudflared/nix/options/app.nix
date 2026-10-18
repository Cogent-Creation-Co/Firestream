# Cloudflared chart options: the connector Deployment pod-spec
# (FLAT -- top-level keys).
#
# This is a NET-NEW Firestream chart, not a Bitnami fork, but it deliberately
# mirrors the flat Bitnami value shape used by nginx/nextjs/odoo: pod-spec
# knobs (replicaCount, containerPorts, probes, resources, security contexts,
# affinity/tolerations, autoscaling, pdb) live at the TOP LEVEL of values.yaml
# rather than nested under `app.*`. Keeping the shape uniform is what lets the
# generated values.yaml stay a faithful sparse override.
#
# serviceAccount lives in its own module; the tunnel surface
# (existingSecret / tunnelTokenSecretKey / metrics / extraArgs) lives in
# options/tunnel.nix.
#
# ABSENT ON PURPOSE: no `service` (the connector only dials OUT -- nothing ever
# connects to it), no persistence, no ingress, no volumePermissions.
#
# NOTE ON DEFAULTS: every option here defaults to null (Model A) INCLUDING
# `replicaCount`, even though the connector's documented default is 2. The 2
# lives in the chart's own values.yaml. Baking it into Nix as well would make
# the generated overlay a non-empty restatement of chart defaults and break the
# cloudflared-render-fidelity check, which asserts the Firestream overlay is a
# no-op over the bare chart.
{ lib, chartTypes, ... }:

let
  t = chartTypes;
  inherit (lib) mkOption types;

  # chartTypes.probeType is a CLOSED submodule carrying only the kubelet timing
  # knobs; this chart's probes also carry a `path` (the HTTP path on the
  # metrics port). Rather than widen the shared type for every chart, we take
  # it freeform here and add `path` explicitly.
  probeWithPathType = types.submodule {
    freeformType = types.attrsOf types.anything;

    options = {
      enabled = mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = "Enable this probe";
      };

      path = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          HTTP path probed on the `metrics` container port. Chart default
          /ready -- cloudflared answers 200 there once at least one connection
          to the Cloudflare edge is registered, 503 otherwise.
        '';
        example = "/ready";
      };

      initialDelaySeconds = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = "Initial delay before the probe starts";
      };

      periodSeconds = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = "How often to perform the probe";
      };

      timeoutSeconds = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = "Probe timeout in seconds";
      };

      failureThreshold = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          Consecutive failures before the probe acts.

          Keep the LIVENESS threshold slack (chart default 6 x 10s = 60s):
          Cloudflare edge blips and reconnect backoff are normal operation, and
          restarting on the first failure turns a transient network event into
          a CrashLoop.
        '';
      };

      successThreshold = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = "Consecutive successes before the probe reports healthy";
      };
    };
  };
in {
  options.cloudflared = {
    # ----- Replicas / ports -----
    replicaCount = mkOption {
      type = types.nullOr types.int;
      default = null;
      description = ''
        Number of connector replicas (chart default 2).

        Cloudflare load-balances a tunnel across every connector registered
        against it, so 2 gives a genuinely redundant edge. This is INDEPENDENT
        of any application's replica count -- that independence is why
        cloudflared is its own Deployment and never a sidecar. A sidecar would
        scale with its app, register duplicate connectors, and could only front
        its own pod.
      '';
      example = 2;
    };

    containerPorts = mkOption {
      type = types.nullOr (types.attrsOf (types.either types.str types.int));
      default = null;
      description = ''
        Container ports. Only `metrics` (chart default 2000) exists: the
        connector accepts no inbound application traffic, so the diagnostic
        server is the sole listener. It is what `--metrics 0.0.0.0:<port>` binds
        and what the probes target.
      '';
      example = { metrics = 2000; };
    };

    extraContainerPorts = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra container ports for the cloudflared container";
    };

    # ----- Resources / security contexts -----
    resourcesPreset = mkOption {
      type = t.resourcesPreset;
      default = null;
      description = ''
        Resource preset (none/nano/micro/small/medium/large/xlarge/2xlarge).
        The chart sets this to `none` and ships explicit `resources` instead,
        which take precedence.
      '';
    };

    resources = mkOption {
      type = types.nullOr t.resourceRequirements;
      default = null;
      description = ''
        Resource requirements (overrides resourcesPreset). Chart default is
        50m/64Mi requests, 200m/128Mi limits -- production-proven for a
        connector, and explicit because GKE Autopilot mutates any pod without
        them.
      '';
    };

    podSecurityContext = mkOption {
      type = types.nullOr t.podSecurityContext;
      default = null;
      description = "Pod security context (chart default: fsGroup 65532, matching the distroless image's nonroot user)";
    };

    containerSecurityContext = mkOption {
      type = types.nullOr t.containerSecurityContext;
      default = null;
      description = ''
        Container security context. Chart default is GKE Autopilot-compatible
        hardening: runAsNonRoot, UID/GID 65532, readOnlyRootFilesystem, no
        privilege escalation, all capabilities dropped, RuntimeDefault seccomp.
      '';
    };

    # ----- Probes (extended set -- each carries a `path`) -----
    livenessProbe = mkOption {
      type = types.nullOr probeWithPathType;
      default = null;
      description = "Liveness probe on cloudflared's /ready (see `path`). Requires `metrics.enabled`.";
    };

    readinessProbe = mkOption {
      type = types.nullOr probeWithPathType;
      default = null;
      description = "Readiness probe on cloudflared's /ready. Requires `metrics.enabled`.";
    };

    startupProbe = mkOption {
      type = types.nullOr probeWithPathType;
      default = null;
      description = "Startup probe on cloudflared's /ready. Requires `metrics.enabled`.";
    };

    customLivenessProbe = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Raw Kubernetes liveness probe that overrides the default one";
    };

    customReadinessProbe = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Raw Kubernetes readiness probe that overrides the default one";
    };

    customStartupProbe = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Raw Kubernetes startup probe that overrides the default one";
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
      description = "Mount the ServiceAccount token in the pod (chart default false -- the connector never calls the Kubernetes API)";
    };

    hostAliases = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Pod host aliases";
    };

    podLabels = mkOption {
      type = types.nullOr (types.attrsOf types.str);
      default = null;
      description = "Extra labels for cloudflared pods";
    };

    podAnnotations = mkOption {
      type = types.nullOr (types.attrsOf types.str);
      default = null;
      description = "Annotations for cloudflared pods";
    };

    command = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      description = "Override the container command (the upstream image's ENTRYPOINT is `cloudflared --no-autoupdate`)";
    };

    args = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      description = ''
        Fully override the container args, REPLACING the chart's
        `tunnel --no-autoupdate [--metrics ...] run`. Prefer `extraArgs`, which
        adds flags while keeping the subcommand intact.
      '';
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
      description = ''
        Pod anti-affinity preset, ignored if `affinity` is set. Chart default
        `soft`: the replicas prefer separate nodes so a single node loss cannot
        take the namespace's edge offline, while still scheduling on a one-node
        dev cluster. Set `hard` on a real multi-node cluster.
      '';
    };

    nodeAffinityPreset = mkOption {
      type = types.nullOr t.nodeAffinityPreset;
      default = null;
      description = "Node affinity preset (ignored if `affinity` is set)";
    };

    affinity = mkOption {
      type = types.nullOr (types.attrsOf types.anything);
      default = null;
      description = "Affinity for pod assignment. Set, it replaces ALL THREE presets above outright.";
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
      description = ''
        Topology spread constraints. An alternative to
        `podAntiAffinityPreset` when the replicas should be spread across zones
        rather than nodes; the two compose, so set the preset to "" if you want
        spreading governed by this alone.
      '';
    };

    priorityClassName = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = ''
        Priority class. Worth setting on a busy cluster: if the connector is
        evicted the namespace loses its edge entirely, whereas evicting one app
        replica only degrades that app.
      '';
    };

    schedulerName = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Use an alternate scheduler";
    };

    terminationGracePeriodSeconds = mkOption {
      type = types.nullOr (types.either types.str types.int);
      default = null;
      description = ''
        Grace period before SIGKILL (chart default 60). cloudflared drains
        in-flight requests on SIGTERM and unregisters from the edge; cutting
        that short drops live connections during a rollout.
      '';
    };

    updateStrategy = mkOption {
      type = types.nullOr t.updateStrategyType;
      default = null;
      description = "Deployment update strategy";
    };

    # ----- Extra volumes / sidecars / init containers -----
    extraVolumes = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra volumes for cloudflared pods";
    };

    extraVolumeMounts = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = "Extra volume mounts for the cloudflared container";
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

    # ----- Env passthrough -----
    extraEnvVars = mkOption {
      type = types.nullOr (types.listOf types.attrs);
      default = null;
      description = ''
        Extra environment variables, appended after TUNNEL_TOKEN. Do NOT set
        TUNNEL_TOKEN here -- the chart already wires it from `existingSecret`,
        and a duplicate env name is rejected by the API server.
      '';
      example = [ { name = "TUNNEL_TRANSPORT_PROTOCOL"; value = "quic"; } ];
    };

    extraEnvVarsCM = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Name of a ConfigMap with extra environment variables";
    };

    extraEnvVarsSecret = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Name of a Secret with extra environment variables";
    };

    # ----- PDB / autoscaling -----
    pdb = mkOption {
      type = types.nullOr t.pdbType;
      default = null;
      description = ''
        Pod Disruption Budget (chart default: not created). Only meaningful
        once the cluster has more nodes than connector replicas -- otherwise it
        blocks node drains forever.
      '';
    };

    autoscaling = mkOption {
      default = null;
      description = ''
        Horizontal pod autoscaling. Rarely appropriate: connector load is
        driven by tunnel traffic, not CPU, and each replica is an edge
        registration -- scale it deliberately via `replicaCount`.
      '';
      type = types.nullOr (types.submodule {
        freeformType = types.attrsOf types.anything;
        options = {
          enabled = mkOption {
            type = types.nullOr types.bool;
            default = null;
            description = "Enable horizontal pod autoscaling (disables the static replicaCount)";
          };
          minReplicas = mkOption {
            type = types.nullOr types.int;
            default = null;
            description = "Minimum number of replicas";
          };
          maxReplicas = mkOption {
            type = types.nullOr types.int;
            default = null;
            description = "Maximum number of replicas";
          };
          targetCPU = mkOption {
            type = types.nullOr (types.either types.str types.int);
            default = null;
            description = "Target CPU utilization percentage";
          };
          targetMemory = mkOption {
            type = types.nullOr (types.either types.str types.int);
            default = null;
            description = "Target memory utilization percentage";
          };
        };
      });
    };
  };
}
