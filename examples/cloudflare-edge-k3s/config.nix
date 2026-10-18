# ---------------------------------------------------------------------------
# YOUR local deployment configuration. The single source of truth for all three
# chart overrides (firestream/*.nix) and the deploy script
# (scripts/deploy-local.sh).
#
# This example differs from the single-app ones in that it deploys THREE charts
# into one namespace: an app, the proxy in front of it, and the tunnel connector
# in front of that.
# ---------------------------------------------------------------------------
{
  # --- Target namespace ----------------------------------------------------
  # All three releases land here. `bin/deploy` creates it (the bundles ship
  # `_meta.createNamespace = true`, the default); a Pulumi-owned deployment
  # would set that false and create the namespace itself, alongside the
  # ResourceQuota / LimitRange / NetworkPolicy that belong with it.
  namespace = "edge-demo";

  # --- The public hostname -------------------------------------------------
  # nginx matches this as `server_name`, so a request only reaches Odoo if it
  # arrives with this Host header. In production it is the hostname you point a
  # Cloudflare DNS record at; locally, `make smoke` supplies it with `curl -H`.
  hostname = "odoo.example.test";

  # --- Storage -------------------------------------------------------------
  # k3s and k3d both ship the rancher.io/local-path provisioner as the default.
  storageClass = "local-path";

  # --- Cluster DNS (optional) ----------------------------------------------
  # Empty (the default) means nginx resolves each upstream name ONCE at config
  # load. That is fine here because deploy-local.sh deploys Odoo first, so its
  # Service already exists -- but it also means nginx REFUSES TO START if a
  # backend is missing, which is a bad property for an edge proxy in a cluster
  # where things restart independently.
  #
  # Set this to the cluster's kube-dns ClusterIP to switch to request-time
  # resolution instead: nginx then boots with nothing behind it and answers 502
  # until the backend appears. It also turns on `qualifyUpstreams`, because
  # nginx's resolver does NOT apply /etc/resolv.conf search domains -- a bare
  # `odoo` would NXDOMAIN forever.
  #
  #   k3s / k3d        10.43.0.10
  #   kubeadm / kind   10.96.0.10
  #   GKE              auto-allocated; `kubectl -n kube-system get svc kube-dns`
  clusterDns = "";

  # --- Odoo server mode ----------------------------------------------------
  # THE setting the websocket path hangs on, and the reason it lives here
  # rather than in odoo-overrides.nix: it has to be agreed by TWO charts.
  #
  #   0   threaded. ONE process bound to 8069, serving websockets on that same
  #       port. 8072 is written into odoo.conf and then ignored -- nothing
  #       listens on it.
  #   > 0 prefork. Spawns a separate gevent worker, and THAT is what binds 8072.
  #
  # odoo-overrides.nix feeds this straight to `odoo.workers`; nginx-overrides.nix
  # derives the /websocket route's port from it (8072 when > 0, else 8069). Both
  # failure directions used to be silent: route 8072 with workers = 0 and every
  # websocket is a 502; omit the route with workers > 0 and websockets ride
  # `location /` with the non-websocket read timeout and get cut periodically.
  # Deriving both from this one number removes the disagreement entirely.
  #
  # Two is the smallest genuinely-prefork value. If you change it, revisit
  # `resourcesPreset` in odoo-overrides.nix -- each worker is a full process.
  odooWorkers = 2;

  # --- Odoo credentials (inline) -------------------------------------------
  # Local example: passwords live here, not in a secret manager. Setting them
  # explicitly gives a KNOWN admin login and lets `helm upgrade` re-deploy
  # without Bitnami's "provide your current passwords when upgrading" guard.
  odooPassword = "admin1234";
  dbPassword = "odoo";
  dbPostgresPassword = "odoo-admin";

  # --- Cloudflare tunnel ---------------------------------------------------
  # The Secret the connector reads its token from. This chart NEVER creates it
  # and never templates the value -- it only references the Secret by name, so
  # the token stays with whatever provisioned the tunnel.
  #
  # Locally there is no tunnel to provision, so deploy-local.sh writes a
  # SYNTACTICALLY VALID FAKE token into this Secret. See the README: the
  # connector will start, fail to register with the Cloudflare edge, and never
  # report Ready. That is the correct and expected outcome -- everything BEHIND
  # the connector is fully exercised, and the one thing that cannot work
  # locally fails honestly rather than being stubbed out.
  tunnelSecretName = "cloudflared-tunnel-token";
}
