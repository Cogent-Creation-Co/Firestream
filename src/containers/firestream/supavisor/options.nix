# Supavisor Container Options
# Copyright Firestream. MIT License.
#
# The Supabase connection pooler: Postgres session mode on 5432 and
# transaction mode on 6543. Consumed by
# bin/nix/firestream/containers/eval-container.nix (runtimeType "system").
#
# Defaults are the upstream self-hosted v0.8.3 compose values. Secrets
# default to empty: compose and the chart supply them, and they accept the
# `_FILE` suffix. An empty DATABASE_URL is built at boot from the POSTGRES_*
# values, as the upstream compose file does.
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.supavisor = {
    version = lib.mkDefault "2.9.13";

    image.repository = lib.mkDefault "firestream-supabase-supavisor";

    paths = {
      base = lib.mkDefault "/opt/firestream/supavisor";
      conf = lib.mkDefault "/opt/firestream/supavisor/config";
      data = lib.mkDefault "/firestream/supavisor/data";
      logs = lib.mkDefault "/opt/firestream/supavisor/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      PORT = "4000";
      PROXY_PORT_SESSION = "5432";
      PROXY_PORT_TRANSACTION = "6543";

      POSTGRES_HOST = "db";
      POSTGRES_PORT = "5432";
      POSTGRES_DB = "postgres";
      POSTGRES_PASSWORD = "";
      # Supavisor's own metadata database (`_supabase`, as supabase_admin).
      DATABASE_URL = "";
      CLUSTER_POSTGRES = "true";

      SECRET_KEY_BASE = "";
      VAULT_ENC_KEY = "";
      API_JWT_SECRET = "";
      METRICS_JWT_SECRET = "";
      REGION = "local";
      ERL_AFLAGS = "-proto_dist inet_tcp";
      PHX_SERVER = "true";

      # The pooler tenant that pooler.exs seeds. Clients log in as
      # `<user>.<POOLER_TENANT_ID>`.
      POOLER_TENANT_ID = "firestream";
      POOLER_DEFAULT_POOL_SIZE = "20";
      POOLER_MAX_CLIENT_CONN = "100";
      POOLER_POOL_MODE = "transaction";
      DB_POOL_SIZE = "5";
    };

    envSecrets = lib.mkDefault [
      "POSTGRES_PASSWORD"
      "DATABASE_URL"
      "SECRET_KEY_BASE"
      "VAULT_ENC_KEY"
      "API_JWT_SECRET"
      "METRICS_JWT_SECRET"
    ];

    exposedPorts = lib.mkDefault [ 4000 5432 6543 ];

    health = {
      enable = lib.mkDefault true;
      # /api/health answers 204.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/''${PORT:-4000} && printf "GET /api/health HTTP/1.0\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 20[04]"' '';
    };
  };
}
