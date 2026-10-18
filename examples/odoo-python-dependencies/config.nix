# ---------------------------------------------------------------------------
# YOUR local deployment configuration. The single source of truth for the Nix
# chart override (firestream/odoo-overrides.nix) and the deploy script
# (scripts/deploy-local.sh).
#
# The extra Python dependencies are NOT configured here; they are the
# ./python-workspace uv2nix workspace, merged into the Odoo image's own venv
# via firestream/odoo-image-overrides.nix (config.odoo.pythonWorkspace.extend).
# ---------------------------------------------------------------------------
{
  namespace = "odoo";

  # k3s/k3d default provisioner.
  storageClass = "local-path";

  # --- Credentials (inline) ------------------------------------------------
  # Local example: passwords live here, NOT in Secret Manager. Explicit values
  # give a known admin login AND keep `helm upgrade` idempotent.
  odooPassword = "admin1234"; # Odoo admin (login: see odooEmail in the override)
  dbPassword = "odoo"; # PostgreSQL application user (firestream)
  dbPostgresPassword = "odoo-admin"; # PostgreSQL superuser (postgres)

  # --- Production-shaped odoo.conf limits ----------------------------------
  # Rendered into the pod env by the chart (each knob is emitted only when set)
  # and from there into odoo.conf by the container on every pod start. Sized
  # for a 2-worker pod with a ~4 GiB memory request; scale with `workers`.
  odooLimits = {
    workers = 2;
    maxCronThreads = 1;
    limitTimeCpu = 600;
    limitTimeReal = 1200;
    limitTimeRealCron = 600;
    limitMemorySoft = 1073741824; # 1 GiB: recycle a worker after its request
    limitMemoryHard = 1610612736; # 1.5 GiB: kill it on the spot
    limitRequest = 8192;
    listDb = false; # never expose the database manager in production
  };
}
