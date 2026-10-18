# cloudflared chart override — the tunnel connector.
#
# Deliberately almost empty, and that is the point. The connector is configured
# REMOTELY: the tunnel's ingress rules live in Cloudflare (declared through the
# Cloudflare API, normally by the same IaC that creates the tunnel), and the pod
# is handed nothing but a token. So there is no config.yaml, no credentials
# file, no ConfigMap and no ingress rules here -- the entire contract between
# this chart and the layer that provisions the tunnel is the name of one Secret.
{ ... }:

let cfg = import ../config.nix;
in
{
  config.cloudflared = {
    # The Secret holding the tunnel token. The chart references it by name and
    # never sees the value. The secretKeyRef is NOT marked optional, so if the
    # Secret is missing the pod stays in CreateContainerConfigError rather than
    # silently running tokenless.
    existingSecret = cfg.tunnelSecretName;
    tunnelTokenSecretKey = "tunnel-token";

    # One replica locally. The chart defaults to 2, which is right in
    # production -- Cloudflare load-balances a tunnel across every connector
    # registered against it, so two pods give a genuinely redundant edge -- but
    # on a single-node k3d cluster it just doubles the pods that will sit
    # not-Ready waiting for a Cloudflare account that does not exist. See the
    # README.
    replicaCount = 1;
  };
}
