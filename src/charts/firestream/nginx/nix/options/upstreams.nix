# Nginx chart options: `upstreams.*` -- THE HEADLINE OPTION.
#
# A typed attrset that drives the ConfigMap's generated `server { }` blocks.
# One nginx server block per attribute, matched on `server_name <hostname>`:
#
#   config.nginx.upstreams = {
#     odoo = {
#       hostname = "odoo.example.com";
#       default  = { service = "odoo"; port = 8069; };
#       routes   = [
#         { paths = [ "/websocket" "/longpolling" ]; service = "odoo"; port = 8072; }
#       ];
#     };
#   };
#
# Semantics:
#   * `hostname`  -> `server_name`
#   * `default`   -> the catch-all `location / { proxy_pass ... }`
#   * `routes[]`  -> one `location <path>` per entry in `paths`, pointing at a
#                    different service/port.
#
# WHY THIS SHAPE (the Odoo case that motivated the whole app):
# Odoo splits traffic across 8069 (HTTP) and 8072 (gevent: websocket +
# longpolling) and expects a reverse proxy to route between them; and
# src/containers/firestream/odoo/module.nix bakes `proxy_mode = True`, so Odoo
# TRUSTS X-Forwarded-For / X-Forwarded-Proto -- which, until this chart, nothing
# was sending. The generated config supplies both (see the "nginx.proxyDirectives"
# helper in ../../templates/_helpers.tpl).
#
# STABILITY CONTRACT: `hostname` is deliberately a PLAIN SCALAR at the values
# path `upstreams.<name>.hostname`. A later phase retargets a deployment branch
# by merging a one-line values overlay at exactly that path, so it must stay
# simple and predictable -- do not nest it, do not make it a list, do not
# compute it from siblings.
#
# NOTE on null-stripping: every leaf below has a CONCRETE default rather than
# Model A's `nullOr + null`. `routes` is a LIST, and the values emitter's
# recursive null-strip does not descend into list elements -- a null default
# inside a route would be serialised as a literal `null` into values.yaml and
# reach the Go template. Concrete defaults keep the emitted YAML clean.
{ lib, ... }:

let
  inherit (lib) mkOption types;

  # A backend target: a Kubernetes Service name (in this namespace) and a port.
  backendType = types.submodule {
    freeformType = types.attrsOf types.anything;

    options = {
      service = mkOption {
        type = types.str;
        description = ''
          Name of the Kubernetes Service to proxy to. Resolved by cluster DNS
          from within the namespace, so a bare Service name is enough
          (e.g. "odoo"); a fully-qualified name works too.
        '';
        example = "odoo";
      };

      port = mkOption {
        type = types.int;
        description = "Port on the target Service.";
        example = 8069;
      };
    };
  };

  # One extra `location` group: a set of paths routed to a different backend
  # than the upstream's `default`.
  routeType = types.submodule {
    freeformType = types.attrsOf types.anything;

    options = {
      paths = mkOption {
        type = types.listOf types.str;
        description = ''
          nginx location paths handled by this route. Each becomes its own
          `location <path> { }` block. Standard nginx prefix-match semantics
          apply (the longest matching prefix wins over `location /`).
        '';
        example = [ "/websocket" "/longpolling" ];
      };

      service = mkOption {
        type = types.str;
        description = "Name of the Kubernetes Service these paths proxy to.";
        example = "odoo";
      };

      port = mkOption {
        type = types.int;
        description = "Port on the target Service.";
        example = 8072;
      };

      websocket = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether this route carries long-lived / upgraded connections.

          When true the location gets `proxy.websocketReadTimeout` /
          `websocketSendTimeout` (300s by default) and `proxy_buffering off`
          instead of the ordinary 60s timeouts. The `Upgrade` /
          `Connection: $connection_upgrade` headers are emitted on every
          location regardless -- that is standard nginx practice and inert for
          non-upgrade requests.

          DEFAULTS TO TRUE. `routes` exists to peel special sub-paths off the
          default backend, and the motivating (and only) case is
          websocket/longpolling. A plain HTTP route loses nothing from the
          longer timeout, whereas a websocket route silently handed the 60s
          default BREAKS Odoo longpolling, which holds connections ~55s. Set
          false to opt out.
        '';
      };
    };
  };

  upstreamType = types.submodule {
    freeformType = types.attrsOf types.anything;

    options = {
      hostname = mkOption {
        type = types.str;
        description = ''
          The `server_name` this block matches -- the public hostname clients
          use. See the STABILITY CONTRACT note at the top of this file: keep it
          a plain scalar at `upstreams.<name>.hostname`.
        '';
        example = "odoo.example.com";
      };

      default = mkOption {
        type = backendType;
        description = ''
          The catch-all backend: becomes `location / { proxy_pass ... }`. Every
          request to this hostname that no `routes` entry claims goes here.
        '';
        example = { service = "odoo"; port = 8069; };
      };

      routes = mkOption {
        type = types.listOf routeType;
        default = [ ];
        description = ''
          Additional `location` blocks pointing at a different service/port than
          `default`. For Odoo this is how /websocket and /longpolling reach the
          gevent worker on 8072 instead of the HTTP worker on 8069.
        '';
        example = [
          { paths = [ "/websocket" "/longpolling" ]; service = "odoo"; port = 8072; }
        ];
      };
    };
  };
in
{
  options.nginx.upstreams = mkOption {
    type = types.nullOr (types.attrsOf upstreamType);
    default = null;
    description = ''
      Virtual hosts this proxy serves, keyed by a short name. Each becomes one
      nginx `server { }` block. Null (the default) emits nothing, leaving the
      chart's own `upstreams: {}` in force -- the proxy then answers every
      request with a 404 from its default server.
    '';
    example = {
      odoo = {
        hostname = "odoo.example.com";
        default = { service = "odoo"; port = 8069; };
        routes = [
          { paths = [ "/websocket" "/longpolling" ]; service = "odoo"; port = 8072; }
        ];
      };
    };
  };
}
