# ---------------------------------------------------------------------------
# YOUR deployment configuration. The single source of truth for all three chart
# overrides (firestream/*.nix), the deploy script, and Cloud Build.
#
# Values marked (from Pulumi) are stack outputs — run `make pulumi-up`, then
# `pulumi stack output` and copy them here.
#
# Four values MUST agree with the Pulumi stack config, because they name the
# same objects from two sides. They are called out individually below.
# ---------------------------------------------------------------------------
{
  # --- GCP (from Pulumi) ---------------------------------------------------
  projectId = "my-gcp-project";
  region = "us-central1";
  clusterName = "firestream-edge";
  arHost = "us-central1-docker.pkg.dev";
  arRepo = "my-gcp-project/firestream";
  imageTag = "latest";

  # --- MUST MATCH the Pulumi stack ----------------------------------------
  # `pulumi config get namespace`. Pulumi creates this namespace and puts the
  # tunnel-token Secret in it; the charts deploy into it.
  namespace = "edge";

  # `pulumi config get hostname`. Cloudflare routes this hostname to the
  # tunnel; nginx matches it as `server_name`. A mismatch gives you a tunnel
  # that resolves and an nginx that answers 404 to everything.
  hostname = "odoo.example.com";

  # `pulumi config get tunnelSecretName` (default below). The Secret Pulumi
  # writes the tunnel token into, and the one the cloudflared chart reads.
  tunnelSecretName = "cloudflared-tunnel-token";

  # --- Cluster DNS ---------------------------------------------------------
  # GKE auto-allocates the Services CIDR, so there is NO correct default —
  # recent Autopilot clusters land around 34.118.224.10, older ones at
  # 10.96.0.10. Read the live value:
  #
  #   kubectl -n kube-system get svc kube-dns -o jsonpath='{.spec.clusterIP}'
  #
  # Setting it makes nginx resolve upstreams at REQUEST time, so the proxy
  # boots and stays up while Odoo is absent or rolling instead of refusing to
  # start. On a real cluster that is the behaviour you want; leave it empty
  # only if you are deliberately relying on deploy ordering.
  clusterDns = "34.118.224.10";

  # --- Storage -------------------------------------------------------------
  storageClass = "standard-rwo";

  # --- Kubernetes Secrets (created by scripts/sync-secrets.sh) -------------
  # NOT created by Helm. sync-secrets.sh materialises them from GCP Secret
  # Manager at deploy time; the charts only reference them by name.
  #
  # The tunnel token is the exception — Pulumi writes that one directly,
  # because Cloudflare mints it as an output of the tunnel resource. See
  # pulumi/__main__.py.
  odooSecretName = "odoo-credentials";
  dbSecretName = "odoo-db-credentials";
}
