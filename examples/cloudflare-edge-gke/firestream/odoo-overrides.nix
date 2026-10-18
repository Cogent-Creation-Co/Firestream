# Odoo chart override — the app behind the proxy (GKE shape).
#
# Same `workers = 2` story as the k3s sibling; the differences are all
# infrastructure: registry-hosted images, Secret Manager credentials referenced
# by name, and Pulumi-owned namespace lifecycle.
{ ... }:

let cfg = import ../config.nix;
in
{
  config.odoo = {
    # Pulumi creates the Namespace, so Helm must not. See ../pulumi/__main__.py.
    _meta.createNamespace = false;

    # ---- THE SETTING THAT MAKES /websocket WORK ---------------------------
    # Odoo binds its gevent port (8072) ONLY in prefork mode. With the chart
    # default of 0 it runs threaded, nothing listens on 8072, and the
    # /websocket route in nginx-overrides.nix is a 502 — pages load fine and
    # only live-update features break.
    #
    # One setting is enough: the chart gates the container port, the Service
    # port and the NetworkPolicy ingress rule on this same value.
    workers = 2;

    # Service port 8069 so "the port nginx talks to" and "the port Odoo listens
    # on" are the same number everywhere. The gevent port needs no entry — the
    # chart publishes `service.ports.gevent` (8072) because `workers` > 0.
    service.ports.http = 8069;
    service.type = "ClusterIP";

    # No Ingress. The ONLY path in is the Cloudflare tunnel, which reaches
    # nginx, which reaches this.
    ingress.enabled = false;

    # Credentials by reference. The chart never sees the values; the Secrets are
    # materialised from Secret Manager by ../scripts/sync-secrets.sh.
    existingSecret = cfg.odooSecretName;

    image = {
      registry = cfg.arHost;
      repository = "${cfg.arRepo}/firestream-odoo";
      tag = cfg.imageTag;
    };

    persistence = {
      enabled = true;
      storageClass = cfg.storageClass;
      size = "20Gi";
    };

    postgresql = {
      enabled = true;
      architecture = "standalone";
      auth = {
        username = "bn_odoo";
        database = "bitnami_odoo";
        existingSecret = cfg.dbSecretName;
      };
      image = {
        registry = cfg.arHost;
        repository = "${cfg.arRepo}/firestream-postgresql";
        tag = cfg.imageTag;
      };
      primary.persistence = {
        enabled = true;
        storageClass = cfg.storageClass;
        size = "20Gi";
      };
    };

    replicaCount = 1;
  };
}
