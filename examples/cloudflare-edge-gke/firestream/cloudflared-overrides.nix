# cloudflared chart override — the tunnel connector (GKE shape).
#
# Almost empty, and that is the point. The tunnel's ingress rules live in
# Cloudflare (created by ../pulumi/__main__.py as a
# ZeroTrustTunnelCloudflaredConfig), so the pod holds nothing but a token and
# this chart's entire contract is the name of one Secret.
{ ... }:

let cfg = import ../config.nix;
in
{
  config.cloudflared = {
    # Pulumi creates the Namespace — and, unusually, the Secret too: Cloudflare
    # mints the tunnel token as an output of the tunnel resource, so it never
    # exists anywhere a shell script could read it from.
    _meta.createNamespace = false;

    existingSecret = cfg.tunnelSecretName;
    tunnelTokenSecretKey = "tunnel-token";

    # Left at the chart's default of 2 deliberately. Cloudflare load-balances a
    # tunnel across every connector registered against it, so two pods give a
    # genuinely redundant edge — and this is the component whose failure takes
    # the whole namespace off the internet.
    replicaCount = 2;
  };
}
