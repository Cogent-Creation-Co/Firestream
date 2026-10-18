# ---------------------------------------------------------------------------
# The "make it your own" surface for LOCAL k3s/k3d — the Helm CHART side.
# Passed as the LAST module to `firestream.lib.<sys>.charts.odoo.eval`, so
# every value here deep-merges on top of the Firestream defaults and the
# image-injection overlay. Intentionally *sparse*.
#
# Same local-cluster shape as ../odoo-k3s (bundled PostgreSQL, inline
# credentials, local-path storage, ClusterIP + port-forward, image refs left
# alone so the side-loaded custom image is used), PLUS the production-shaped
# odoo.conf limits from config.nix. Option names mirror
# src/charts/firestream/odoo/values.yaml (flat top-level keys).
# ---------------------------------------------------------------------------
{ ... }:

let
  cfg = import ../config.nix;
in
{
  config.odoo = {
    odooEmail = "user@example.com";
    odooPassword = cfg.odooPassword;

    postgresql = {
      enabled = true;
      architecture = "standalone";
      auth = {
        username = "firestream";
        database = "firestream_odoo";
        password = cfg.dbPassword;
        postgresPassword = cfg.dbPostgresPassword;
      };
      primary.persistence = {
        enabled = true;
        storageClass = cfg.storageClass;
        size = "10Gi";
      };
    };

    persistence = {
      enabled = true;
      storageClass = cfg.storageClass;
      size = "10Gi";
    };

    service.type = "ClusterIP";
    ingress.enabled = false;
    replicaCount = 1;
    resourcesPreset = "small";

    # --- odoo.conf limits (see config.nix) ---------------------------------
    # `workers > 0` switches Odoo to prefork mode, which is what makes the
    # per-worker memory limits meaningful (and binds the gevent port 8072).
    inherit (cfg.odooLimits)
      workers maxCronThreads
      limitTimeCpu limitTimeReal limitTimeRealCron
      limitMemorySoft limitMemoryHard limitRequest
      listDb;
  };
}
