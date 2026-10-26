# Supabase chart options: the per-component domains of values.yaml.
#
# values.yaml groups settings by domain, then by component
# (`deployment.<c>`, `image.<c>`, `environment.<c>`, `service.<c>`,
# `autoscaling.<c>`, `serviceAccount.<c>`), so these options do the same.
# Every leaf defaults to null and null leaves are stripped, so the emitted
# values.yaml stays a sparse overlay over the chart's own defaults. Probes,
# security contexts and volumes are raw Kubernetes objects, as in the chart.
{ lib, chartTypes, ... }:
let
  inherit (lib) mkOption types;
  t = chartTypes;

  components = [ "db" "auth" "rest" "realtime" "supavisor" "meta" "storage" "imgproxy" "studio" "envoy" ];

  opt = type: description: mkOption { type = types.nullOr type; default = null; inherit description; };
  raw = types.attrsOf types.anything;
  rawList = types.listOf types.anything;

  sub = options: description: mkOption {
    default = null;
    inherit description;
    type = types.nullOr (types.submodule { freeformType = raw; inherit options; });
  };

  deployment = c: sub ({
    enabled = opt types.bool "Render the ${c} workload.";
    replicaCount = opt types.int "Replica count (ignored when autoscaling.${c}.enabled).";
    nameOverride = opt types.str "Override the component name.";
    fullnameOverride = opt types.str "Override the full resource name.";
    labels = opt (types.attrsOf types.str) "Extra workload labels.";
    annotations = opt (types.attrsOf types.str) "Extra workload annotations.";
    podAnnotations = opt (types.attrsOf types.str) "Extra pod annotations.";
    podSecurityContext = opt raw "Pod securityContext.";
    securityContext = opt raw "Container securityContext.";
    automountServiceAccountToken = opt types.bool "Mount the ServiceAccount token.";
    terminationGracePeriodSeconds = opt types.int "Pod termination grace period.";
    nodeSelector = opt (types.attrsOf types.str) "Pod nodeSelector.";
    affinity = opt raw "Pod affinity.";
    tolerations = opt (types.listOf t.tolerationType) "Pod tolerations.";
    priorityClassName = opt types.str "Pod priorityClassName.";
    envFrom = opt rawList "Container envFrom sources.";
    livenessProbe = opt raw "Container livenessProbe.";
    readinessProbe = opt raw "Container readinessProbe (default: healthd /readyz on 9180).";
    startupProbe = opt raw "Container startupProbe.";
    lifecycle = opt raw "Container lifecycle hooks.";
    resources = opt t.resourceRequirements "Container resource requests and limits.";
    volumeMounts = opt rawList "Extra container volumeMounts.";
    volumes = opt rawList "Extra pod volumes.";
  }
  // lib.optionalAttrs (c != "envoy" && c != "meta" && c != "imgproxy" && c != "studio") {
    extraInitContainers = opt rawList "Extra init containers.";
  }
  // lib.optionalAttrs (lib.elem c [ "auth" "rest" "realtime" "supavisor" "storage" ]) {
    initDb = opt types.bool "Wait for the database in an init container.";
  }
  // lib.optionalAttrs (lib.elem c [ "auth" "realtime" "supavisor" ]) {
    dnsPolicy = opt types.str "Pod dnsPolicy.";
    dnsConfig = opt raw "Pod dnsConfig.";
  }
  // lib.optionalAttrs (lib.elem c [ "realtime" "supavisor" ]) {
    command = opt (types.listOf types.str) "Container command override.";
    args = opt (types.listOf types.str) "Container args override.";
  }
  // lib.optionalAttrs (c == "studio") {
    strategy = opt raw "Deployment strategy.";
  }
  // lib.optionalAttrs (c == "envoy") {
    runDir = opt types.str "Writable directory for the rendered Envoy config (ENVOY_RUN_DIR).";
  }
  // lib.optionalAttrs (c == "storage") {
    createBucket = opt types.bool "Create GLOBAL_S3_BUCKET in an init container before Storage starts.";
  }) "Pod settings of the ${c} component.";

  image = c: sub {
    registry = opt types.str "Image registry; empty means no prefix.";
    repository = opt types.str "Image repository.";
    tag = opt types.str "Image tag.";
    pullPolicy = opt (types.enum [ "Always" "IfNotPresent" "Never" ]) "Image pull policy.";
    pullSecrets = opt rawList "Image pull secrets.";
  } "Image of the ${c} component. Filled by image injection.";

  service = c: sub ({
    type = opt (types.enum [ "ClusterIP" "NodePort" "LoadBalancer" "ExternalName" ]) "Service type.";
    port = opt types.port "Service port.";
  } // lib.optionalAttrs (c == "supavisor") {
    sessionPort = opt types.port "Session-mode pooler port.";
    transactionPort = opt types.port "Transaction-mode pooler port.";
  }) "Service of the ${c} component.";

  autoscaling = c: sub {
    enabled = opt types.bool "Create a HorizontalPodAutoscaler.";
    minReplicas = opt types.int "Minimum replicas.";
    maxReplicas = opt types.int "Maximum replicas.";
    targetCPUUtilizationPercentage = opt types.int "Target CPU utilisation.";
    targetMemoryUtilizationPercentage = opt types.int "Target memory utilisation.";
  } "Autoscaling of the ${c} component.";

  serviceAccount = c: sub {
    create = opt types.bool "Create a ServiceAccount.";
    annotations = opt (types.attrsOf types.str) "ServiceAccount annotations.";
    name = opt types.str "ServiceAccount name.";
  } "ServiceAccount of the ${c} component.";

  envEntry = types.submodule {
    freeformType = raw;
    options.name = mkOption { type = types.str; description = "Variable name."; };
  };

  perComponent = f: lib.genAttrs components f;
in
{
  options.supabase = {
    deployment = perComponent deployment;
    image = perComponent image;
    environment = perComponent (c: opt (types.listOf envEntry)
      "Extra env of the ${c} component: { name, value } or { name, valueFrom } entries. Replaces the chart's list.");
    service = perComponent service;
    autoscaling = perComponent autoscaling;
    serviceAccount = perComponent serviceAccount;
  };
}
