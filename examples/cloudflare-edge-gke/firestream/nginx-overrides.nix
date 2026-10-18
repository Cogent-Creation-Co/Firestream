# nginx chart override — the per-namespace edge proxy (GKE shape).
{ lib, ... }:

let
  cfg = import ../config.nix;
  useResolver = cfg.clusterDns != "";
in
{
  config.nginx = {
    # Pulumi creates the Namespace, so Helm must not.
    _meta.createNamespace = false;

    # This is the Service the Cloudflare tunnel's ingress rule points at:
    #   http://nginx.<namespace>.svc.cluster.local:80
    # (see ../pulumi/__main__.py). ClusterIP is correct and deliberate — there
    # is no LoadBalancer and no public port anywhere in this deployment.
    service.type = "ClusterIP";

    upstreams.odoo = {
      hostname = cfg.hostname;
      default = {
        service = "odoo";
        port = 8069;
      };
      # Odoo's websocket / longpolling traffic is served by the gevent worker on
      # 8072, not by the HTTP workers on 8069. `websocket` defaults to true,
      # raising this location's timeouts to 300s — Odoo longpolling holds a
      # connection ~55s, so the ordinary 60s read timeout cuts polls off
      # mid-flight.
      routes = [
        {
          paths = [ "/websocket" "/longpolling" ];
          service = "odoo";
          port = 8072;
        }
      ];
    };

    # Odoo runs with `proxy_mode = True` baked in, so it TRUSTS X-Forwarded-Proto
    # to decide whether it is being served over HTTPS. TLS terminates at the
    # Cloudflare edge and the request reaches this proxy over plain HTTP, so the
    # chart forwards the edge's header rather than $scheme. Without that, every
    # absolute URL Odoo generates would point at http://.
    proxy.clientMaxBodySize = "100m";

    # Two replicas: this is the single component every request flows through, so
    # a rollout or node drain with one replica is a full outage.
    replicaCount = 2;

    image = {
      registry = cfg.arHost;
      repository = "${cfg.arRepo}/firestream-nginx";
      tag = cfg.imageTag;
    };
  } // lib.optionalAttrs useResolver {
    nginxConfig = {
      resolver = cfg.clusterDns;
      # Not optional alongside a resolver: nginx's resolver ignores
      # /etc/resolv.conf search domains, so the bare `odoo` above must be
      # expanded to `odoo.<namespace>.svc.cluster.local`. The chart does that
      # when it renders, where the namespace is known.
      qualifyUpstreams = true;
    };
  };
}
