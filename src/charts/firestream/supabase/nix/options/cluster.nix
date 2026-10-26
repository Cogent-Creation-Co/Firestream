# Supabase chart options: the chart-wide domains of values.yaml
# (persistence, ingress, networkPolicy, migration, serviceMonitor, names).
{ lib, ... }:
let
  inherit (lib) mkOption types;
  opt = type: description: mkOption { type = types.nullOr type; default = null; inherit description; };
  raw = types.attrsOf types.anything;
  sub = options: description: mkOption {
    default = null;
    inherit description;
    type = types.nullOr (types.submodule { freeformType = raw; inherit options; });
  };
  pvc = name: sub {
    enabled = opt types.bool "Create and mount a PersistentVolumeClaim for ${name}.";
    existingClaim = opt types.str "Use this existing claim instead.";
    storageClassName = opt types.str "StorageClass of the claim.";
    annotations = opt (types.attrsOf types.str) "Claim annotations.";
    size = opt types.str "Requested size.";
    accessModes = opt (types.listOf types.str) "Claim access modes.";
  } "Volume of ${name}.";
in
{
  options.supabase = {
    nameOverride = opt types.str "Override the chart name.";
    fullnameOverride = opt types.str "Override the full release name.";

    persistence = sub {
      db = pvc "the database (data directory and pgsodium root key)";
      imgproxy = pvc "imgproxy";
      studio = pvc "Studio (snippets and edge function files)";
    } "PersistentVolumeClaims.";

    ingress = sub {
      enabled = opt types.bool "Create an Ingress for the gateway.";
      className = opt types.str "IngressClass name.";
      annotations = opt (types.attrsOf types.str) "Ingress annotations.";
      tls = opt (types.listOf raw) "Ingress TLS entries.";
      hosts = opt (types.listOf raw) "Ingress host rules.";
    } "Ingress of the Envoy gateway.";

    networkPolicy = sub {
      db = sub {
        enabled = opt types.bool "Restrict ingress to the database.";
        ingressRules = opt (types.listOf raw) "Allowed peers ({ from = ...; }).";
      } "Database NetworkPolicy.";
    } "NetworkPolicies.";

    migration = opt (types.attrsOf types.str)
      "SQL files run once on the first database boot, after the Supabase migrations.";

    serviceMonitor = sub {
      enabled = opt types.bool "Create ServiceMonitors.";
      components = opt raw "Per-component ServiceMonitor settings.";
    } "Prometheus ServiceMonitors.";
  };
}
