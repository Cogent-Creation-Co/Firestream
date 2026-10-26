# PostgREST Container Options
# Copyright Firestream. MIT License.
#
# The Supabase REST API (`/rest/v1`). Consumed by
# bin/nix/firestream/containers/eval-container.nix (runtimeType "system").
#
# Secrets (PGRST_DB_URI holds the authenticator password, PGRST_JWT_SECRET)
# default to empty: compose and the chart supply them from
# `firestreamLib.supabase.devSecrets` or a generated set, and both accept the
# `_FILE` suffix.
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.postgrest = {
    # NOTE: the pin's pkgs.postgrest. Upstream v0.8.3 runs v14.18; 14.1 reads
    # every PGRST_* key that the upstream compose file sets except
    # PGRST_DB_USE_LEGACY_GUCS, which `--dump-config` does not list.
    version = lib.mkDefault "14.1";

    image.repository = lib.mkDefault "firestream-supabase-postgrest";

    paths = {
      base = lib.mkDefault "/opt/firestream/postgrest";
      conf = lib.mkDefault "/opt/firestream/postgrest/config";
      data = lib.mkDefault "/firestream/postgrest/data";
      logs = lib.mkDefault "/opt/firestream/postgrest/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      PGRST_DB_URI = "";
      PGRST_DB_SCHEMAS = "public,graphql_public";
      PGRST_DB_ANON_ROLE = "anon";
      PGRST_DB_MAX_ROWS = "1000";
      PGRST_DB_EXTRA_SEARCH_PATH = "public";
      PGRST_JWT_SECRET = "";
      PGRST_DB_USE_LEGACY_GUCS = "false";
      PGRST_APP_SETTINGS_JWT_EXP = "3600";
      PGRST_SERVER_PORT = "3000";
      # NOTE: localhost keeps the admin API off the network; the readiness
      # probe asks it from inside the container.
      PGRST_ADMIN_SERVER_HOST = "localhost";
      PGRST_ADMIN_SERVER_PORT = "3001";
    };

    envSecrets = lib.mkDefault [
      "PGRST_DB_URI"
      "PGRST_JWT_SECRET"
    ];

    exposedPorts = lib.mkDefault [ 3000 ];

    health = {
      enable = lib.mkDefault true;
      # NOTE: not `postgrest --ready` (the upstream probe): 14.1 refuses it
      # unless server-host is "localhost", which would close the API port.
      # healthd runs outside the entrypoint env, so the port has a fallback.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/''${PGRST_ADMIN_SERVER_PORT:-3001} && printf "GET /ready HTTP/1.0\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 200"' '';
    };
  };
}
