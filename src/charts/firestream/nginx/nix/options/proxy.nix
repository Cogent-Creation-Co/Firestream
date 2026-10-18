# Nginx chart options: `proxy.*` and `nginxConfig.*`.
#
# `proxy.*`       - behaviour applied to every generated `location` block.
# `nginxConfig.*` - nginx core / http-level directives.
#
# Model A: nullOr leaves defaulting to null, so an unset knob is stripped from
# the emitted values.yaml and the chart's own default wins.
{ lib, ... }:

let
  inherit (lib) mkOption types;
in
{
  options.nginx.proxy = mkOption {
    default = null;
    description = "Proxy behaviour applied to every generated location block";
    type = types.nullOr (types.submodule {
      freeformType = types.attrsOf types.anything;

      options = {
        clientMaxBodySize = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = ''
            `client_max_body_size`. nginx defaults to 1m, which is far too small
            for Odoo attachment uploads; the chart raises it to 100m.
          '';
          example = "100m";
        };

        connectTimeout = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "`proxy_connect_timeout` for every location (chart default 30s)";
          example = "30s";
        };

        readTimeout = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = ''
            `proxy_read_timeout` for NON-websocket locations (chart default
            180s). Deliberately above the Firestream Odoo image's baked
            `limit_time_real = 150`: at nginx's own 60s default the proxy 504s
            first and the user sees a gateway error for a request Odoo goes on
            to complete. Keep this above whatever hard request limit the
            upstream app enforces.
          '';
          example = "180s";
        };

        sendTimeout = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "`proxy_send_timeout` for NON-websocket locations (chart default 180s); kept symmetric with `readTimeout`";
          example = "180s";
        };

        websocketReadTimeout = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = ''
            `proxy_read_timeout` for routes with `websocket = true` (chart
            default 300s). Odoo longpolling holds a connection open ~55s per
            poll, so anything at or below 60s cuts polls off mid-flight.
          '';
          example = "300s";
        };

        websocketSendTimeout = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "`proxy_send_timeout` for routes with `websocket = true` (chart default 300s)";
          example = "300s";
        };

        healthPath = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = ''
            Path of the local, non-proxying 200 endpoint (chart default
            /healthz). It backs the Deployment's probes AND the container's
            `health.readinessCmd`, so changing it here means changing it in
            src/containers/firestream/nginx/options.nix too.
          '';
          example = "/healthz";
        };
      };
    });
  };

  options.nginx.nginxConfig = mkOption {
    default = null;
    description = "nginx core / http-level configuration";
    type = types.nullOr (types.submodule {
      freeformType = types.attrsOf types.anything;

      options = {
        workerProcesses = mkOption {
          type = types.nullOr (types.either types.str types.int);
          default = null;
          description = "`worker_processes` (chart default \"auto\")";
          example = "auto";
        };

        workerConnections = mkOption {
          type = types.nullOr types.int;
          default = null;
          description = "`worker_connections` per worker (chart default 1024)";
          example = 1024;
        };

        errorLogLevel = mkOption {
          type = types.nullOr (types.enum [ "debug" "info" "notice" "warn" "error" "crit" "alert" "emerg" ]);
          default = null;
          description = "Severity threshold for the error log, which goes to stderr (chart default warn)";
          example = "warn";
        };

        accessLog = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Write the access log to stdout (chart default true)";
        };

        tmpDir = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = ''
            Writable scratch directory: nginx's pid file plus every
            client/proxy/fastcgi temp path. The pod runs with
            readOnlyRootFilesystem: true and an emptyDir at /tmp, so this must
            stay under /tmp and must match the container's NGINX_TMP_DIR
            (src/containers/firestream/nginx/options.nix). Chart default
            /tmp/nginx.
          '';
          example = "/tmp/nginx";
        };

        keepaliveTimeout = mkOption {
          type = types.nullOr (types.either types.str types.int);
          default = null;
          description = "`keepalive_timeout` (chart default 65)";
        };

        serverTokens = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Emit the nginx version in error pages and the Server header (chart default false)";
        };

        resolver = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = ''
            DNS server for REQUEST-TIME upstream resolution (chart default "",
            i.e. disabled).

            Empty means `proxy_pass http://<svc>:<port>;` -- nginx resolves each
            name once at config load and REFUSES TO START if a Service does not
            exist yet ("host not found in upstream"). That is why nginx is
            ordered after its upstreams in firestreamStacks.dev.

            Set it to the cluster DNS address (typically 10.96.0.10, or the
            `nameserver` from a pod's /etc/resolv.conf) to switch to the
            deferred form -- `set $fs_upstream ...; proxy_pass
            http://$fs_upstream$request_uri;` -- which lets the proxy start and
            stay up while a backend is absent, answering 502 until it returns.
          '';
          example = "10.96.0.10";
        };

        resolverValid = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "How long resolved upstream addresses are cached (chart default 30s). Only used when `resolver` is set.";
          example = "30s";
        };

        resolverTimeout = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "`resolver_timeout` (chart default 5s). Only used when `resolver` is set.";
          example = "5s";
        };

        qualifyUpstreams = mkOption {
          type = types.nullOr types.bool;
          default = null;
          example = true;
          description = ''
            Expand dotless `upstreams.*.service` names to
            `<svc>.<release namespace>.svc.<clusterDomain>` when the chart
            renders (chart default false). Names already containing a dot are
            treated as fully-qualified and passed through.

            SET THIS WHENEVER THE VALUES ARE GENERATED WITHOUT KNOWING THE
            NAMESPACE -- the normal case for a Nix-built bundle whose namespace
            the deploying layer chooses later. Values strings are not
            `tpl`-rendered, so `{{ .Release.Namespace }}` cannot be written into
            `upstreams` directly; this resolves the name when the chart renders,
            where the namespace is known.

            Orthogonal to `resolver`: that chooses WHEN a name is resolved,
            this chooses WHAT name is resolved.
          '';
        };

        extraHttpConfig = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = ''
            Raw nginx directives spliced into the `http { }` block, after the
            maps and before the server blocks. Escape hatch for anything not
            modelled above.
          '';
        };

        extraServerConfig = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Raw nginx directives spliced into EVERY generated `server { }` block";
        };
      };
    });
  };
}
