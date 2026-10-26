# postgres-meta Container Options
# Copyright Firestream. MIT License.
#
# The Postgres management API that Studio calls (`/pg/` through the gateway).
# Consumed by bin/nix/firestream/containers/eval-container.nix (runtimeType
# "system").
#
# Defaults are the upstream self-hosted v0.8.3 compose values. Secrets
# (PG_META_DB_PASSWORD, CRYPTO_KEY) default to empty: compose and the chart
# supply them from `firestreamLib.supabase.devSecrets` or a generated set
# (CRYPTO_KEY is PG_META_CRYPTO_KEY there), and both accept the `_FILE`
# suffix.
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.pgmeta = {
    version = lib.mkDefault "0.100.0";

    image.repository = lib.mkDefault "firestream-supabase-postgres-meta";

    paths = {
      base = lib.mkDefault "/opt/firestream/pgmeta";
      conf = lib.mkDefault "/opt/firestream/pgmeta/config";
      data = lib.mkDefault "/firestream/pgmeta/data";
      logs = lib.mkDefault "/opt/firestream/pgmeta/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      PG_META_HOST = "0.0.0.0";
      PG_META_PORT = "8080";
      PG_META_DB_HOST = "db";
      PG_META_DB_PORT = "5432";
      PG_META_DB_NAME = "postgres";
      PG_META_DB_USER = "postgres";
      PG_META_DB_PASSWORD = "";
      # Encrypts the connection string that Studio sends in a request header.
      CRYPTO_KEY = "";
    };

    envSecrets = lib.mkDefault [
      "PG_META_DB_PASSWORD"
      "CRYPTO_KEY"
    ];

    exposedPorts = lib.mkDefault [ 8080 ];

    health = {
      enable = lib.mkDefault true;
      # healthd runs outside the entrypoint env, so the port has a fallback.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/''${PG_META_PORT:-8080} && printf "GET /health HTTP/1.0\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 200"' '';
    };
  };
}
