# Supabase Studio Container Options
# Copyright Firestream. MIT License.
#
# The Supabase dashboard. Consumed by
# bin/nix/firestream/containers/eval-container.nix (runtimeType "system").
#
# Defaults are the upstream self-hosted v0.8.3 compose values. Secrets
# (POSTGRES_PASSWORD, PG_META_CRYPTO_KEY, the API keys, AUTH_JWT_SECRET)
# default to empty: compose and the chart supply them from
# `firestreamLib.supabase.devSecrets` or a generated set, and they accept the
# `_FILE` suffix.
#
# Studio keeps SQL snippets and edge function sources as files. Both folders
# sit under paths.data, which needs a writable volume.
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.studio = {
    version = lib.mkDefault "2026.10.12";

    image.repository = lib.mkDefault "firestream-supabase-studio";

    paths = {
      base = lib.mkDefault "/opt/firestream/studio";
      conf = lib.mkDefault "/opt/firestream/studio/config";
      data = lib.mkDefault "/firestream/studio/data";
      logs = lib.mkDefault "/opt/firestream/studio/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      # The entrypoint exports it as HOSTNAME, the Next.js bind address.
      STUDIO_BIND_ADDRESS = "0.0.0.0";
      PORT = "3000";
      NEXT_TELEMETRY_DISABLED = "1";

      STUDIO_PG_META_URL = "http://meta:8080";
      POSTGRES_HOST = "db";
      POSTGRES_PORT = "5432";
      POSTGRES_DB = "postgres";
      POSTGRES_PASSWORD = "";
      POSTGRES_USER_READ_WRITE = "postgres";
      PG_META_CRYPTO_KEY = "";

      PGRST_DB_SCHEMAS = "public,storage,graphql_public";
      PGRST_DB_MAX_ROWS = "1000";
      PGRST_DB_EXTRA_SEARCH_PATH = "public";

      DEFAULT_ORGANIZATION_NAME = "Default Organization";
      DEFAULT_PROJECT_NAME = "Default Project";

      # The gateway address inside the stack, and the one a browser uses.
      SUPABASE_URL = "http://api-gw:8000";
      SUPABASE_PUBLIC_URL = "http://localhost:8000";
      SUPABASE_ANON_KEY = "";
      SUPABASE_SERVICE_KEY = "";
      SUPABASE_PUBLISHABLE_KEY = "";
      SUPABASE_SECRET_KEY = "";
      AUTH_JWT_SECRET = "";

      # Logflare (analytics) is out of scope.
      ENABLED_FEATURES_LOGS_ALL = "false";

      SNIPPETS_MANAGEMENT_FOLDER = "/firestream/studio/data/snippets";
      EDGE_FUNCTIONS_MANAGEMENT_FOLDER = "/firestream/studio/data/functions";
    };

    envSecrets = lib.mkDefault [
      "POSTGRES_PASSWORD"
      "PG_META_CRYPTO_KEY"
      "SUPABASE_ANON_KEY"
      "SUPABASE_SERVICE_KEY"
      "SUPABASE_PUBLISHABLE_KEY"
      "SUPABASE_SECRET_KEY"
      "AUTH_JWT_SECRET"
    ];

    exposedPorts = lib.mkDefault [ 3000 ];

    health = {
      enable = lib.mkDefault true;
      # healthd runs outside the entrypoint env, so the port has a fallback.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/''${PORT:-3000} && printf "GET /api/platform/profile HTTP/1.0\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 200"' '';
    };
  };
}
