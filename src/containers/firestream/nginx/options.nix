# Nginx Container Options (shared base)
# Copyright Firestream. MIT License.
#
# Externalized, declarative configuration for the Firestream nginx container,
# consumed by bin/nix/firestream/containers/eval-container.nix (runtimeType =
# "system"). Defaults here mirror the literals in module.nix so evalContainer's
# default build matches the direct-import path.
#
# nginx is the per-namespace REVERSE PROXY (see module.nix's header for the full
# rationale: Odoo bakes `proxy_mode = True` and splits HTTP/websocket across
# 8069/8072, and nothing was supplying the X-Forwarded-* headers it then trusts).
# It is NOT a Bitnami chart, so there are no per-container helpers / config /
# init scripts - the container is simply "nginx on PATH" plus a baked default
# config that the chart's ConfigMap shadows.
#
# SINGLE-VERSION: `version` tracks nixpkgs' nginx directly rather than a
# Firestream-pinned N/M split.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ lib, pkgs, ... }:

{
  config.nginx = {
    # Tracks nixpkgs' nginx (stable branch); also becomes the image tag.
    version = lib.mkDefault pkgs.nginx.version;

    # Paths configuration (Firestream FHS). Per-key mkDefault so a consumer can
    # move one path without dropping its siblings. The env-defaults generator
    # (bin/nix/firestream/env/defaults.nix) requires all four keys to be present.
    paths = {
      base = lib.mkDefault "/opt/firestream/nginx";
      conf = lib.mkDefault "/opt/firestream/nginx/config";
      data = lib.mkDefault "/firestream/nginx/data";
      logs = lib.mkDefault "/opt/firestream/nginx/logs";
    };

    # Environment variables with defaults.
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    #
    # These four ARE the container<->chart contract. The chart's ConfigMap
    # template hardcodes the same values; changing one here without changing the
    # chart breaks the binding.
    env = builtins.mapAttrs (_: lib.mkDefault) {
      # Where nginx reads its config. The chart mounts its generated ConfigMap
      # over the containing DIRECTORY, shadowing the baked default.
      NGINX_CONF_FILE = "/opt/firestream/nginx/config/nginx.conf";

      # `nginx -p` prefix.
      NGINX_PREFIX = "/opt/firestream/nginx";

      # UNPRIVILEGED listener. Never 80: GKE Autopilot forbids privileged ports
      # and root containers, and the chart runs this pod runAsNonRoot.
      NGINX_HTTP_PORT_NUMBER = "8080";

      # Writable scratch tree (pid file + client/proxy/fastcgi temp paths). The
      # chart backs this with an emptyDir so readOnlyRootFilesystem: true works.
      NGINX_TMP_DIR = "/tmp/nginx";
    };

    # A reverse proxy holds no credentials; nothing needs _FILE secret support.
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [ ];

    # Unprivileged HTTP (8080). When health.enable is true (below),
    # eval-container.nix appends healthd's 9180 to this list automatically, so
    # it must NOT be listed here.
    exposedPorts = lib.mkDefault [ 8080 ];

    # In-image firestream-healthd. Readiness is the /healthz location that both
    # the baked default config and the chart's generated config serve locally,
    # WITHOUT proxying - so readiness reflects "nginx is up and parsing its
    # config", not the health of any backend. That is the correct semantic for a
    # proxy: it must stay Ready (and keep returning its own 502s) even while an
    # upstream is rolling.
    health = {
      enable = lib.mkDefault true;
      readinessCmd = lib.mkDefault
        ''curl -fsS -o /dev/null "http://localhost:''${NGINX_HTTP_PORT_NUMBER:-8080}/healthz"'';
    };

    # Distinct host-port offset (spacing 2000) so every canonical app can run on
    # docker simultaneously without colliding. nginx=38000 (next free slot after
    # nextjs=36000):
    #   nginx   8080 -> host 46080
    #   healthd 9180 -> host 47180
    compose.hostPortOffset = lib.mkDefault 38000;
  };
}
