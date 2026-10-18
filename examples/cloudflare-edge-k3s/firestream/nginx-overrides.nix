# nginx chart override — the per-namespace edge proxy.
#
# `upstreams` is the headline option: one nginx `server { }` block per entry,
# matched on `server_name <hostname>`. This is where the three charts have to
# agree — the service name and ports below must match what odoo-overrides.nix
# actually exposes.
{ lib, ... }:

let
  cfg = import ../config.nix;
  useResolver = cfg.clusterDns != "";

  # The one number both charts have to agree on. Odoo binds 8072 ONLY in prefork
  # mode (`odoo.workers > 0`, set from this same `cfg.odooWorkers`); in threaded
  # mode websockets are served on 8069 and 8072 is dead. Deriving the route port
  # here means the two charts cannot be configured into disagreement -- which
  # was previously possible in both directions, and silent in both.
  odooWebsocketPort = if cfg.odooWorkers > 0 then 8072 else 8069;
in
{
  config.nginx = {
    upstreams.odoo = {
      # The public hostname. A request only reaches Odoo if it arrives with
      # this Host header; anything else falls through to the default server,
      # which answers 404 "no upstream configured for this host".
      hostname = cfg.hostname;

      # The catch-all: `location / { proxy_pass ... }`.
      default = {
        service = "odoo";
        port = 8069;
      };

      # Extra `location` blocks pointing somewhere else. Odoo's websocket and
      # longpolling traffic is served by the gevent worker on 8072, NOT by the
      # HTTP workers on 8069 -- which is why odoo-overrides.nix sets `workers`.
      #
      # `websocket` defaults to true, which raises this location's timeouts to
      # 300s and turns off proxy buffering. Odoo longpolling holds a connection
      # open for ~55s per poll, so the ordinary non-websocket read timeout cuts
      # polls off mid-flight.
      #
      # The route is kept even at `odooWorkers = 0` -- pointing at 8069 in that
      # case -- precisely so the websocket TIMEOUTS still apply. Dropping it
      # would let websockets fall through to `location /` and inherit the
      # non-websocket read timeout, which severs every long poll on a clock.
      routes = [
        {
          paths = [ "/websocket" "/longpolling" ];
          service = "odoo";
          port = odooWebsocketPort;
        }
      ];
    };

    # Odoo runs with `proxy_mode = True` baked into the image, meaning it TRUSTS
    # X-Forwarded-Proto to decide whether it is being served over HTTPS. The
    # chart sends it on every location. Without that, every absolute URL Odoo
    # generates -- password-reset links, portal links, outbound mail -- would
    # point at http://.
    #
    # 100m because Odoo attachment uploads routinely exceed nginx's 1m default.
    proxy.clientMaxBodySize = "100m";
  } // lib.optionalAttrs useResolver {
    nginxConfig = {
      # Request-time DNS: nginx boots even if Odoo's Service is absent and
      # answers 502 until it appears, instead of refusing to start.
      resolver = cfg.clusterDns;
      # Required alongside `resolver`: nginx's resolver ignores
      # /etc/resolv.conf search domains, so the bare `odoo` above has to be
      # expanded to `odoo.<namespace>.svc.cluster.local`. The chart does that
      # when it renders, which is the first point at which the namespace is
      # known.
      qualifyUpstreams = true;
    };
  };
}
