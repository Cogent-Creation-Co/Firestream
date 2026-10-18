# Odoo chart override — the app behind the proxy.
#
# The one line that matters for this example is `workers`. Everything else is
# the ordinary local-k3s shape borrowed from ../../odoo-k3s.
{ ... }:

let cfg = import ../config.nix;
in
{
  config.odoo = {
    # ---- THE SETTING THAT MAKES /websocket WORK ---------------------------
    # Odoo has two server modes exposing DIFFERENT sockets:
    #
    #   workers = 0  threaded; ONE process bound to 8069 only, serving
    #                websockets on that same port. 8072 is written into
    #                odoo.conf and then ignored -- nothing listens on it.
    #   workers > 0  prefork; spawns a separate gevent worker, and THAT is what
    #                binds 8072.
    #
    # nginx-overrides.nix routes /websocket and /longpolling to Odoo's 8072, so
    # without this the whole websocket path is a 502 -- pages load fine and only
    # the live-update features break, which is a miserable thing to debug.
    #
    # Setting `workers` is all it takes: the chart gates the container port, the
    # Service port and the NetworkPolicy ingress rule on the same value, so they
    # cannot drift apart.
    #
    # The number itself lives in ../config.nix, because nginx-overrides.nix has
    # to agree with it -- it derives the /websocket route's upstream port from
    # the same field. See the `odooWorkers` comment there.
    workers = cfg.odooWorkers;

    # Service port 8069 rather than the chart's default 80, so "the port nginx
    # talks to" and "the port Odoo listens on" are the same number everywhere.
    # The gevent port needs no entry here -- the chart publishes
    # `service.ports.gevent` (8072) on the strength of `workers` above.
    service.ports.http = 8069;

    odooPassword = cfg.odooPassword;
    # Odoo is reached only through nginx, which is reached only through the
    # tunnel. No Ingress, no LoadBalancer.
    service.type = "ClusterIP";
    ingress.enabled = false;

    persistence = {
      enabled = true;
      storageClass = cfg.storageClass;
      size = "10Gi";
    };

    postgresql = {
      enabled = true;
      architecture = "standalone";
      auth = {
        password = cfg.dbPassword;
        postgresPassword = cfg.dbPostgresPassword;
      };
      primary.persistence = {
        enabled = true;
        storageClass = cfg.storageClass;
        size = "10Gi";
      };
    };

    # ---- SIZED FOR `workers` ABOVE, not picked at random -------------------
    # `workers = 2` means the pod runs a master process, TWO HTTP worker
    # processes, a gevent worker and a cron thread -- five Odoo processes, each
    # carrying its own Python heap and ORM caches. The `small` preset caps the
    # container at 768Mi, which they exceed under any real use.
    #
    # This matters more than a normal over-commit because Odoo's own
    # `limit_memory_soft` / `limit_memory_hard` are NOT set: nothing recycles an
    # individual worker when it grows. The cgroup limit is the only ceiling, and
    # when it is hit the kernel OOM-kills the POD, not one worker. The symptom
    # is a CrashLoopBackOff that reads as "notifications are flaky" rather than
    # as an out-of-memory problem.
    #
    # `large` (limits 1.5 CPU / 3072Mi) is the smallest stock preset clearing
    # 2Gi -- see src/charts/bitnami/common/templates/_resources.tpl. If you
    # lower `workers`, you can lower this with it.
    resourcesPreset = "large";
    replicaCount = 1;
  };
}
