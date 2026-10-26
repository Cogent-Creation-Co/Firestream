# Supabase Realtime Container Options
# Copyright Firestream. MIT License.
#
# The Supabase Realtime server (`/realtime/v1`): Postgres changes, broadcast
# and presence over websockets. Consumed by
# bin/nix/firestream/containers/eval-container.nix (runtimeType "system").
#
# Defaults are the upstream self-hosted v0.8.3 compose values. Secrets
# default to empty: compose and the chart supply them, and they accept the
# `_FILE` suffix. DB_ENC_KEY takes the `REALTIME_DB_ENC_KEY` secret.
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.realtime = {
    version = lib.mkDefault "2.143.3";

    image.repository = lib.mkDefault "firestream-supabase-realtime";

    paths = {
      base = lib.mkDefault "/opt/firestream/realtime";
      conf = lib.mkDefault "/opt/firestream/realtime/config";
      data = lib.mkDefault "/firestream/realtime/data";
      logs = lib.mkDefault "/opt/firestream/realtime/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      PORT = "4000";
      DB_HOST = "db";
      DB_PORT = "5432";
      DB_USER = "supabase_admin";
      DB_PASSWORD = "";
      DB_NAME = "postgres";
      DB_AFTER_CONNECT_QUERY = "SET search_path TO _realtime";
      DB_ENC_KEY = "";
      DB_IP_VERSION = "ipv4";
      API_JWT_SECRET = "";
      METRICS_JWT_SECRET = "";
      SECRET_KEY_BASE = "";
      ERL_AFLAGS = "-proto_dist inet_tcp";
      # The upstream value. runtime.exs reads it only when
      # CLUSTER_STRATEGIES includes DNS.
      DNS_NODES = "''";
      RLIMIT_NOFILE = "10000";
      APP_NAME = "realtime";
      # Seeds the `realtime-dev` tenant, the name the gateway routes to.
      SEED_SELF_HOST = "true";
      RUN_JANITOR = "true";
      DISABLE_HEALTHCHECK_LOGGING = "true";
      PHX_SERVER = "true";
    };

    envSecrets = lib.mkDefault [
      "DB_PASSWORD"
      "DB_ENC_KEY"
      "API_JWT_SECRET"
      "METRICS_JWT_SECRET"
      "SECRET_KEY_BASE"
    ];

    exposedPorts = lib.mkDefault [ 4000 ];

    health = {
      enable = lib.mkDefault true;
      # NOTE: /healthcheck needs no key. The upstream probe asks the tenant
      # health route, which needs the anon JWT that this container lacks.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/''${PORT:-4000} && printf "GET /healthcheck HTTP/1.0\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 200"' '';
    };
  };
}
