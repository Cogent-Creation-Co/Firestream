# Nginx Container Module - Using Firestream Factories
# Copyright Firestream. MIT License.
#
# Defines the Firestream nginx container using `mkContainerModule`. nginx is the
# per-namespace REVERSE PROXY: it terminates plain HTTP from an upstream edge
# (a Cloudflare tunnel, an LB, or another proxy) and fans requests out to the
# application Services in the same namespace.
#
# WHY THIS EXISTS
# ---------------
# `src/containers/firestream/odoo/module.nix` bakes `proxy_mode = True` into
# odoo.conf unconditionally. With proxy_mode on, Odoo *trusts* X-Forwarded-For /
# X-Forwarded-Proto — but nothing in Firestream was setting them, so Odoo saw no
# forwarded headers at all and fell back to guessing: wrong absolute URLs in
# outbound mail and redirects, and HTTPS mis-detected as HTTP. Odoo also splits
# traffic across two ports (8069 HTTP, 8072 gevent/longpolling+websocket) and
# expects a reverse proxy to route `/websocket` + `/longpolling` to the latter.
# This container, driven by the chart's generated nginx.conf, is that proxy.
#
# Deliberately MINIMAL, in the shape of seaweedfs: nginx is not a Bitnami chart,
# so there are no perContainerHelpers, no validate/config/init scripts, and no
# /opt/bitnami path-remap dance. The container is simply "nginx on PATH" plus a
# self-sufficient default config.
#
# UNPRIVILEGED BY CONSTRUCTION
# ----------------------------
# Listens on 8080, never 80. GKE Autopilot rejects both privileged ports and
# root containers, and the chart runs this pod with runAsNonRoot + a read-only
# root filesystem. Consequently:
#   * the baked config carries NO `user` directive (nginx only honours it as
#     root, and warns otherwise),
#   * every writable path nginx needs (pid file, proxy/client temp dirs) is
#     redirected under $NGINX_TMP_DIR, which the chart backs with an emptyDir,
#   * logs go to /dev/stdout + /dev/stderr rather than a writable logs dir.
#
# CONTAINER <-> CHART CONTRACT
# ----------------------------
# The chart mounts its generated nginx.conf ConfigMap over the *directory*
# `/opt/firestream/nginx/config`, shadowing the baked default below. The
# contract the chart's ConfigMap template must honour is exactly:
#   * config file path        $NGINX_CONF_FILE = /opt/firestream/nginx/config/nginx.conf
#   * listen port             $NGINX_HTTP_PORT_NUMBER = 8080
#   * writable scratch prefix $NGINX_TMP_DIR = /tmp/nginx
#   * a `/healthz` location returning 200 without proxying (readiness probe).
#
# NOTE ON mime.types: this image intentionally ships no mime.types include. A
# reverse proxy relays the upstream's Content-Type verbatim; the only locally
# generated response is /healthz, which sets `default_type text/plain` itself.
# Referencing ${pkgs.nginx}/conf/mime.types from a chart-supplied config would
# require baking a Nix store path into chart YAML, which the chart cannot know.

{ pkgs
, lib
, firestream

  # Pinned to nixpkgs' nginx (stable branch). Default mirrors options.nix.
, version ? pkgs.nginx.version

  # Externalized core-surface config. Defaults below equal the options.nix
  # literals so the direct-import path and evalContainer (which passes the same
  # values from options.nix) yield identical factory args.

, paths ? {
    base = "/opt/firestream/nginx";
    conf = "/opt/firestream/nginx/config";
    data = "/firestream/nginx/data";
    logs = "/opt/firestream/nginx/logs";
  }

, envVars ? {
    NGINX_CONF_FILE = "/opt/firestream/nginx/config/nginx.conf";
    NGINX_PREFIX = "/opt/firestream/nginx";
    NGINX_HTTP_PORT_NUMBER = "8080";
    NGINX_TMP_DIR = "/tmp/nginx";
  }

  # Nothing here is secret-bearing: the proxy holds no credentials. Kept as an
  # explicit empty list so the factory signature matches its siblings.
, envVarsWithSecrets ? [ ]

  # Unprivileged HTTP listener. NOT 80 - see header.
, exposedPorts ? [ 8080 ]

  # Image naming passthrough (parity defaults).
, imageName ? "firestream-nginx"
, imageTag ? version

  # Injected by eval-container.nix only when health.enable is true.
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  # Default, self-sufficient nginx.conf baked into the image. It is what you get
  # from a bare `docker run` / docker-compose, and it is SHADOWED in Kubernetes
  # by the chart's ConfigMap mount over /opt/firestream/nginx/config.
  #
  # It proxies nothing (a standalone proxy has no upstreams to speak of) but it
  # does serve /healthz, so the compose healthcheck and `health.readinessCmd`
  # below both succeed against the bare image.
  defaultNginxConf = ''
    # Firestream nginx - baked default configuration.
    # Copyright Firestream. MIT License.
    #
    # Replaced at deploy time by the chart's generated ConfigMap. Keep this in
    # sync with the invariants documented in module.nix: port 8080, no `user`
    # directive, all writable state under /tmp/nginx.

    worker_processes auto;
    error_log /dev/stderr warn;
    pid /tmp/nginx/nginx.pid;

    events {
      worker_connections 1024;
    }

    http {
      # No mime.types include - see module.nix. Proxied responses carry the
      # upstream's Content-Type; local responses set their own.
      default_type application/octet-stream;

      access_log /dev/stdout;

      # Every writable path redirected under the emptyDir-backed scratch dir so
      # the container works with readOnlyRootFilesystem: true.
      client_body_temp_path /tmp/nginx/client_body;
      proxy_temp_path       /tmp/nginx/proxy;
      fastcgi_temp_path     /tmp/nginx/fastcgi;
      uwsgi_temp_path       /tmp/nginx/uwsgi;
      scgi_temp_path        /tmp/nginx/scgi;

      sendfile on;
      tcp_nopush on;
      keepalive_timeout 65;
      server_tokens off;

      server {
        listen 8080 default_server;
        server_name _;

        location /healthz {
          access_log off;
          default_type text/plain;
          return 200 "ok\n";
        }

        location / {
          default_type text/plain;
          return 404 "no upstream configured\n";
        }
      }
    }
  '';

  # System dependencies (shared libs/tools available in the container).
  systemDeps = with pkgs; [
    cacert
    openssl
    coreutils
    gnugrep
    gnused
    gawk
    findutils
    which
    procps
  ];

  # Runtime binary deps (on PATH at runtime). `nginx` is the headline binary;
  # `curl` backs health.readinessCmd (options.nix) and the compose healthcheck.
  runtimeBinDeps = with pkgs; [
    nginx
    curl
    coreutils
    bash
    gnused
    gnugrep
    gawk
    findutils
    which
  ];

in
firestream.mkContainerModule ({
  name = "nginx";
  inherit version;

  inherit paths envVars envVarsWithSecrets;

  # Image naming passthrough.
  inherit imageName imageTag;

  # Declarative directory schema. A proxy holds no durable state; the only
  # directories that matter are the config dir (shadowed by the ConfigMap in
  # k8s) and the ephemeral scratch tree nginx writes temp bodies into.
  runtimeDirs = {
    conf = {
      path = "/opt/firestream/nginx/config";
      type = "conf";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "nginx configuration directory (shadowed by the chart ConfigMap)";
    };
    tmp = {
      path = "/tmp/nginx";
      type = "tmp";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "nginx scratch dir: pid file + client/proxy temp paths";
    };
  };

  # The baked default config. `prepopulateFiles` writes it at BUILD time and, at
  # runtime, only re-creates it when absent (`if [[ ! -f ... ]]` in
  # apps/base.nix) - so a chart-mounted ConfigMap is never clobbered.
  prepopulateFiles = {
    "/opt/firestream/nginx/config/nginx.conf" = defaultNginxConf;
  };

  # Standalone / docker-compose startup command. In Kubernetes the chart mounts
  # its ConfigMap at $NGINX_CONF_FILE and this same line runs it unchanged.
  #
  # NOTE: the run wrapper emits `exec ''${runCmd}`, so the body MUST start with a
  # comment (making that prefix a harmless no-op `exec`) and end with its own
  # `exec` - the same convention nextjs/redis/spark use.
  #
  # `-e /dev/stderr` overrides the compiled-in default error log, which nginx
  # opens BEFORE parsing the config; without it nginx would try to write into
  # its read-only Nix store prefix during startup.
  runCmd = ''
    # Run nginx in the foreground as PID 1 (daemon off), reading the config the
    # chart's ConfigMap supplies (or the baked default outside Kubernetes).
    mkdir -p "''${NGINX_TMP_DIR:-/tmp/nginx}" 2>/dev/null || true
    exec nginx \
      -c "''${NGINX_CONF_FILE:-/opt/firestream/nginx/config/nginx.conf}" \
      -p "''${NGINX_PREFIX:-/opt/firestream/nginx}" \
      -e /dev/stderr \
      -g "daemon off;"
  '';

  inherit systemDeps runtimeBinDeps;

  inherit exposedPorts;

  # No volumes: the proxy is stateless by design (no PVC in the chart either).
  volumes = [ ];

  user = { name = "nginx"; group = "nginx"; uid = 1001; gid = 1001; };

  # Development shell extras
  devShellPackages = with pkgs; [ docker docker-compose nginx ];
  devShellHook = ''
    echo "Nginx Version: ${version}"
    echo "nginx Binary: ${pkgs.nginx}/bin/nginx"
    echo ""
    echo "Build commands:"
    echo "  nix build .#nginx           - Build the Docker image"
    echo "  docker load < result        - Load image into Docker"
  '';
} // lib.optionalAttrs health.enable { inherit health; })
