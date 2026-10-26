# Supabase variant of the PostgreSQL container
# Copyright Firestream. MIT License.
#
# An options layer on top of ./options.nix. It selects the extension set,
# preload libraries and settings that the Supabase migrations and services
# need. The base options stay untouched, so the stock image is unchanged.
#
# Called as `import ./supabase.nix { supabase = firestream.supabase; }` from
# nix/flake-modules/containers/supabase-postgres.nix.

{ supabase }:

{ lib, pkgs, ... }:

let
  inherit (supabase.postgres) migrations supautilsConf;

  # WARNING: the key decrypts every pgsodium and Vault secret. A key file
  # outside the persistent volume is regenerated on the next boot, and all
  # stored secrets become unreadable.
  getkey = pkgs.writeShellApplication {
    name = "pgsodium-getkey";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      if [ -n "''${POSTGRESQL_PGSODIUM_ROOT_KEY:-}" ]; then
        printf '%s\n' "$POSTGRESQL_PGSODIUM_ROOT_KEY"
        exit 0
      fi
      key_file="''${POSTGRESQL_VOLUME_DIR:-/firestream/postgresql}/pgsodium_root.key"
      if [ ! -s "$key_file" ]; then
        umask 077
        head -c 32 /dev/urandom | od -A n -t x1 | tr -d ' \n' > "$key_file.tmp"
        mv "$key_file.tmp" "$key_file"
      fi
      cat "$key_file"
    '';
  };

  # NOTE: supabase_admin is the bootstrap superuser (initdbSuperuser below),
  # and postgres already exists, which migrate.sh accepts as pre-created. So
  # migrate.sh runs unchanged, and an upstream sync is a pin bump. The
  # password is set first because pg_hba already requires one.
  # WARNING: keep the database owner change. migrate.sh does it only when it
  # creates the postgres role itself. Without it, postgres is not
  # pg_database_owner and cannot create tables in the public schema, which
  # Studio, postgres-meta and the Supabase docs expect.
  migrate = pkgs.writeShellScript "00-supabase-migrate.sh" ''
    set -euo pipefail
    export POSTGRES_HOST=127.0.0.1
    export POSTGRES_PORT="''${POSTGRESQL_PORT_NUMBER:-5432}"
    export POSTGRES_DB=postgres
    export POSTGRES_USER=supabase_admin
    export POSTGRES_PASSWORD="''${POSTGRESQL_PASSWORD:-}"
    export JWT_EXP="''${JWT_EXP:-3600}"
    export PGPASSWORD="$POSTGRES_PASSWORD"
    echo "alter role supabase_admin with password :'pw'; alter database postgres owner to postgres;" \
      | psql -v ON_ERROR_STOP=1 --no-psqlrc -h "$POSTGRES_HOST" -p "$POSTGRES_PORT" -U postgres -d postgres \
          -v pw="$POSTGRES_PASSWORD"
    exec sh ${migrations}/migrate.sh
  '';

  # NOTE: POSTGRESQL_INITSCRIPTS_DIR points at the baked migrations, so
  # without this file a /docker-entrypoint-initdb.d mount would be ignored.
  # It is sourced, so it runs the mount's scripts with the image's helpers.
  userScripts = pkgs.writeText "99-user-init-scripts.sh" ''
    find /docker-entrypoint-initdb.d/ -type f -regex ".*\.\(sh\|sql\|sql.gz\)" 2>/dev/null | sort | while read -r f; do
      case "$f" in
        *.sh) if [[ -x "$f" ]]; then "$f"; else . "$f"; fi ;;
        *.sql) postgresql_execute "$POSTGRESQL_DATABASE" "$POSTGRESQL_USERNAME" "$POSTGRESQL_PASSWORD" < "$f" ;;
        *.sql.gz) gunzip -c "$f" | postgresql_execute "$POSTGRESQL_DATABASE" "$POSTGRESQL_USERNAME" "$POSTGRESQL_PASSWORD" ;;
      esac
    done
  '';

  initScripts = pkgs.runCommand "supabase-postgres-initdb.d" { } ''
    mkdir $out
    cp ${migrate} $out/00-supabase-migrate.sh
    cp ${userScripts} $out/99-user-init-scripts.sh
    chmod 0555 $out/00-supabase-migrate.sh
    chmod 0444 $out/99-user-init-scripts.sh
  '';

  # NOTE: priority 900 sits between the base options' mkDefault (1000) and a
  # consumer's plain definition (100): the variant replaces the stock default,
  # and a consumer can still override it without mkForce.
  variantDefault = lib.mkOverride 900;
in
{
  config.postgresql = {
    image.repository = lib.mkDefault "firestream-supabase-postgres";

    # NOTE: the migrations demote postgres, and Postgres cannot demote the
    # bootstrap superuser.
    initdbSuperuser = variantDefault "supabase_admin";

    # NOTE: Auth and Realtime exit when they start before the migrations end.
    readyAfterInit = variantDefault true;

    extensions = variantDefault (ps: [
      ps.pgsodium
      ps.pg_graphql
      ps.pg_net
      ps.pgjwt
      ps.pg_cron
      ps.pgvector
      ps.wal2json
      ps.plpgsql_check
      ps.pgaudit
      ps.postgis
      ps.pg_repack
      ps.pg_safeupdate
      ps.pg_tle
      ps.pgmq
      ps.pgsql-http
      ps.hypopg
      ps.pgtap
      ps.supautils
      ps.supabase_vault
      ps.plan_filter
    ]);

    # NOTE: wal_level is also set here because first-boot initialization
    # writes no wal_level unless a replication user exists;
    # POSTGRESQL_WAL_LEVEL covers every later boot, where the replication
    # parameters are rewritten.
    extraConf = variantDefault ''
      session_preload_libraries = 'supautils'
      include = '${supautilsConf}'
      pgsodium.getkey_script = '${lib.getExe getkey}'
      vault.getkey_script = '${lib.getExe getkey}'
      cron.database_name = 'postgres'
      wal_level = 'logical'
      max_replication_slots = 10
    '';

    env = {
      POSTGRESQL_SHARED_PRELOAD_LIBRARIES = variantDefault
        "pg_stat_statements, pgaudit, plpgsql, plpgsql_check, pg_cron, pg_net, pgsodium, auto_explain, pg_tle, plan_filter, supabase_vault";
      POSTGRESQL_WAL_LEVEL = variantDefault "logical";
      POSTGRESQL_PGSODIUM_ROOT_KEY = lib.mkDefault "";
      POSTGRESQL_INITSCRIPTS_DIR = variantDefault "${initScripts}";
      # NOTE: every Supabase service role gets this password (roles.sql), so an
      # empty one would leave them all open.
      ALLOW_EMPTY_PASSWORD = variantDefault "no";
      JWT_EXP = lib.mkDefault "3600";
    };

    envSecrets = lib.mkDefault [ "POSTGRESQL_PGSODIUM_ROOT_KEY" ];
  };
}
