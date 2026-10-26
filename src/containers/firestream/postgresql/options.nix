# PostgreSQL Container Options (shared base)
# Copyright Firestream. MIT License.
#
# Externalized, declarative SHARED configuration for the PostgreSQL container,
# consumed by bin/nix/firestream/containers/eval-container.nix. Defaults here are
# lifted VERBATIM from module.nix so that evalContainer's default build is
# byte-for-byte identical to the legacy flake.nix build path.
#
# NOTE: PostgreSQL is MULTI-VERSION (16/17). `version` varies per build and is
# supplied by the flake-module via an inline override module, NOT here.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ config, lib, pkgs, ... }:

let
  # NOTE: healthd starts before the entrypoint exports the env defaults, so
  # the fallback must equal the POSTGRESQL_TMP_DIR default. The tmp dir is
  # not on the data volume, and it is writable wherever the socket is.
  readyMarker = ''"''${POSTGRESQL_TMP_DIR:-/opt/firestream/postgresql/tmp}/.firestream-serving"'';
in
{
  options.postgresql.extensions = lib.mkOption {
    type = lib.types.nullOr (lib.types.functionTo (lib.types.listOf lib.types.package));
    default = null;
    description = ''
      Extensions to build into the server, as a function of the extension set
      (`ps: [ ps.pgvector ps.supabase_vault ]`). The set is nixpkgs'
      `postgresql.pkgs` plus the Firestream-built extensions
      (`firestream.packages.postgresqlExtensionsFor`). null (default) => the
      plain server with no extensions; the stock image is unchanged.
    '';
  };

  options.postgresql.extraConf = lib.mkOption {
    type = lib.types.lines;
    default = "";
    description = ''
      postgresql.conf lines written to `$POSTGRESQL_CONF_DIR/conf.d/firestream.conf`
      on every boot, before first-boot initialization starts the server. Settings
      that the image's own env-driven properties also write (for example
      `shared_preload_libraries`, `wal_level`) belong in the env, because
      postgresql.conf sets them after the conf.d include. Empty (default) => no
      file; the stock image is unchanged.
    '';
  };

  options.postgresql.initdbSuperuser = lib.mkOption {
    type = lib.types.str;
    default = "postgres";
    description = ''
      The bootstrap superuser that initdb creates. Postgres refuses to drop
      SUPERUSER from this role, so a stack that later demotes `postgres` (the
      Supabase migrations do) needs another bootstrap role. Any value other than
      "postgres" also creates a `postgres` superuser right after initdb, because
      the image's init steps connect as `postgres`. "postgres" (default) => the
      stock image is unchanged.
    '';
  };

  options.postgresql.readyAfterInit = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = ''
      Report ready only once the long-running server runs. First-boot
      initialization serves its init scripts from a temporary server on the
      same address and port, and a plain `pg_isready` accepts it, so a
      dependent can start before the init scripts end. true => every boot
      removes a marker, the entrypoint writes it just before it starts the
      long-running server, and the readiness command requires it. false
      (default) => the stock image is unchanged.
    '';
  };

  config.postgresql = {
    # extraModuleArgs seam (eval-container.nix splices this into moduleArgs).
    extraModuleArgs = {
      inherit (config.postgresql) extensions extraConf initdbSuperuser;
      readyMarker = if config.postgresql.readyAfterInit then readyMarker else null;
    };

    # Paths configuration (Bitnami compatibility)
    # Per-key mkDefault so individual paths can be overridden independently.
    paths = {
      base = lib.mkDefault "/opt/firestream/postgresql";
      conf = lib.mkDefault "/opt/firestream/postgresql/conf";
      data = lib.mkDefault "/firestream/postgresql/data";
      logs = lib.mkDefault "/opt/firestream/postgresql/logs";
    };

    # Environment variables with defaults
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    env = builtins.mapAttrs (_: lib.mkDefault) {
      # Base directories (pre-expanded to avoid alphabetical ordering issues)
      POSTGRESQL_BASE_DIR = "/opt/firestream/postgresql";
      POSTGRESQL_VOLUME_DIR = "/firestream/postgresql";
      POSTGRESQL_DATA_DIR = "/firestream/postgresql/data";
      POSTGRESQL_CONF_DIR = "/opt/firestream/postgresql/conf";
      POSTGRESQL_MOUNTED_CONF_DIR = "/firestream/postgresql/conf";
      POSTGRESQL_DEFAULT_CONF_DIR = "/opt/firestream/postgresql/conf.default";
      POSTGRESQL_CONF_FILE = "/opt/firestream/postgresql/conf/postgresql.conf";
      POSTGRESQL_PGHBA_FILE = "/opt/firestream/postgresql/conf/pg_hba.conf";
      POSTGRESQL_LOG_DIR = "/opt/firestream/postgresql/logs";
      POSTGRESQL_LOG_FILE = "/opt/firestream/postgresql/logs/postgresql.log";
      POSTGRESQL_TMP_DIR = "/opt/firestream/postgresql/tmp";
      POSTGRESQL_PID_FILE = "/opt/firestream/postgresql/tmp/postgresql.pid";
      POSTGRESQL_INITSCRIPTS_DIR = "/docker-entrypoint-initdb.d";
      POSTGRESQL_PREINITSCRIPTS_DIR = "/docker-entrypoint-preinitdb.d";

      # User and group
      POSTGRESQL_DAEMON_USER = "postgres";
      POSTGRESQL_DAEMON_GROUP = "postgres";

      # Connection settings
      POSTGRESQL_PORT_NUMBER = "5432";
      POSTGRESQL_ALLOW_REMOTE_CONNECTIONS = "yes";
      POSTGRESQL_INIT_MAX_TIMEOUT = "60";

      # Authentication
      # Default to ALLOW_EMPTY_PASSWORD=yes so the out-of-the-box image runs
      # in any local/dev/e2e environment without requiring a password override.
      # Production deployers override these via the standard mkDefault seam.
      #
      # The Bitnami postgresql chart injects credentials under docker-style
      # `POSTGRES_*` env names, but every init/helper script here reads the
      # `POSTGRESQL_*` prefix. mkEnvDefaults emits `export VAR="${VAR:-<value>}"`,
      # so embedding a `${POSTGRES_*:-default}` fallback reproduces the alias
      # bridge upstream Bitnami ships in its `postgresql-env.sh`. Each reference
      # is `:-`-guarded (safe under `set -u`). The `_FILE` aliases are exported
      # before the secrets file-loader runs so chart-mounted password files win.
      # KEEP IN SYNC with module.nix's envVars default.
      POSTGRESQL_PASSWORD = "\${POSTGRES_PASSWORD:-}";
      POSTGRESQL_USERNAME = "\${POSTGRES_USER:-postgres}";
      POSTGRESQL_DATABASE = "\${POSTGRES_DATABASE:-}";
      POSTGRESQL_POSTGRES_PASSWORD = "\${POSTGRES_POSTGRES_PASSWORD:-}";
      POSTGRESQL_PASSWORD_FILE = "\${POSTGRES_PASSWORD_FILE:-}";
      POSTGRESQL_POSTGRES_PASSWORD_FILE = "\${POSTGRES_POSTGRES_PASSWORD_FILE:-}";
      POSTGRESQL_INITSCRIPTS_USERNAME = "\${POSTGRES_INITSCRIPTS_USERNAME:-}";
      POSTGRESQL_INITSCRIPTS_PASSWORD = "\${POSTGRES_INITSCRIPTS_PASSWORD:-}";
      ALLOW_EMPTY_PASSWORD = "yes";

      # Replication (values alias the chart's `POSTGRES_*` names — see above)
      POSTGRESQL_REPLICATION_MODE = "\${POSTGRES_REPLICATION_MODE:-}";
      POSTGRESQL_REPLICATION_USER = "\${POSTGRES_REPLICATION_USER:-}";
      POSTGRESQL_REPLICATION_PASSWORD = "\${POSTGRES_REPLICATION_PASSWORD:-}";
      POSTGRESQL_REPLICATION_PASSWORD_FILE = "\${POSTGRES_REPLICATION_PASSWORD_FILE:-}";
      POSTGRESQL_MASTER_HOST = "\${POSTGRES_MASTER_HOST:-}";
      POSTGRESQL_MASTER_PORT_NUMBER = "\${POSTGRES_MASTER_PORT_NUMBER:-5432}";
      POSTGRESQL_NUM_SYNCHRONOUS_REPLICAS = "\${POSTGRES_NUM_SYNCHRONOUS_REPLICAS:-0}";
      POSTGRESQL_SYNCHRONOUS_COMMIT_MODE = "\${POSTGRES_SYNCHRONOUS_COMMIT_MODE:-on}";
      POSTGRESQL_CLUSTER_APP_NAME = "\${POSTGRES_CLUSTER_APP_NAME:-walreceiver}";
      POSTGRESQL_WAL_LEVEL = "replica";
      POSTGRESQL_REPLICATION_USE_PASSFILE = "no";
      POSTGRESQL_REPLICATION_PASSFILE_PATH = "/opt/firestream/postgresql/conf/.pgpass";

      # TLS
      POSTGRESQL_ENABLE_TLS = "no";
      POSTGRESQL_TLS_CERT_FILE = "";
      POSTGRESQL_TLS_KEY_FILE = "";
      POSTGRESQL_TLS_CA_FILE = "";
      POSTGRESQL_TLS_CRL_FILE = "";
      POSTGRESQL_TLS_PREFER_SERVER_CIPHERS = "yes";

      # LDAP
      POSTGRESQL_ENABLE_LDAP = "no";
      POSTGRESQL_LDAP_URL = "";
      POSTGRESQL_LDAP_SERVER = "";
      POSTGRESQL_LDAP_PORT = "";
      POSTGRESQL_LDAP_PREFIX = "";
      POSTGRESQL_LDAP_SUFFIX = "";
      POSTGRESQL_LDAP_BASE_DN = "";
      POSTGRESQL_LDAP_BIND_DN = "";
      POSTGRESQL_LDAP_BIND_PASSWORD = "";
      POSTGRESQL_LDAP_SEARCH_ATTR = "";
      POSTGRESQL_LDAP_SEARCH_FILTER = "";
      POSTGRESQL_LDAP_TLS = "";
      POSTGRESQL_LDAP_SCHEME = "";

      # Other settings
      POSTGRESQL_SHUTDOWN_MODE = "fast";
      POSTGRESQL_PGCTLTIMEOUT = "60";
      POSTGRESQL_FIRST_BOOT = "yes";
      # Alias the chart's `POSTGRES_INITDB_*` names (note WALDIR vs WAL_DIR).
      POSTGRESQL_INITDB_ARGS = "\${POSTGRES_INITDB_ARGS:-}";
      POSTGRESQL_INITDB_WAL_DIR = "\${POSTGRES_INITDB_WALDIR:-}";
      POSTGRESQL_SHARED_PRELOAD_LIBRARIES = "";
      POSTGRESQL_FSYNC = "on";
      POSTGRESQL_DEFAULT_TOAST_COMPRESSION = "";
      POSTGRESQL_PASSWORD_ENCRYPTION = "";

      # NSS wrapper
      NSS_WRAPPER_LIB = "${pkgs.nss_wrapper}/lib/libnss_wrapper.so";

      # Debug mode
      BITNAMI_DEBUG = "false";
    };

    # Variables supporting _FILE suffix for Docker secrets.
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
      "POSTGRESQL_PASSWORD"
      "POSTGRESQL_POSTGRES_PASSWORD"
      "POSTGRESQL_REPLICATION_PASSWORD"
      "POSTGRESQL_TLS_CERT_FILE"
      "POSTGRESQL_TLS_KEY_FILE"
      "POSTGRESQL_TLS_CA_FILE"
      "POSTGRESQL_TLS_CRL_FILE"
      "POSTGRESQL_LDAP_URL"
      "POSTGRESQL_LDAP_BIND_PASSWORD"
      "POSTGRESQL_INITDB_ARGS"
      "POSTGRESQL_SHARED_PRELOAD_LIBRARIES"
    ];

    exposedPorts = lib.mkDefault [ 5432 ];

    # Phase 3 pilot / Phase 4 generalisation: enable the in-image
    # firestream-healthd service for postgresql. The entrypoint wrapper
    # (base.nix) launches healthd in the background; /readyz invokes
    # `pg_isready` to track real serviceability, not just listener-bound.
    #
    # The readinessCmd is passed verbatim to `firestream-healthd
    # --readiness-cmd ...`, which evaluates it via `sh -c` at probe time —
    # so `${POSTGRESQL_USERNAME:-postgres}` expands at runtime against the
    # container's own env. This is required for the airflow-embedded
    # postgres, which overrides POSTGRESQL_USERNAME=airflow and
    # POSTGRESQL_DATABASE=airflow; a hard-coded `-U postgres -d postgres`
    # would fail on that deployment. The `:-postgres` defaults keep the
    # standalone case working even if those vars are unset.
    health = {
      enable = lib.mkDefault true;
      # `pg_isready` defaults to `/run/postgresql` for its socket directory.
      # The firestream-postgresql container sockets at POSTGRESQL_TMP_DIR
      # (default `/opt/firestream/postgresql/tmp`), and chart consumers
      # override POSTGRESQL_TMP_DIR to a writable emptyDir mount. In neither
      # case does the default socket directory exist. Pin TCP host+port for
      # a deterministic, cross-deployment probe (matches the Bitnami
      # chart's own readinessProbe pattern: `pg_isready -h 127.0.0.1 -p 5432`).
      readinessCmd = lib.mkDefault
        (lib.optionalString config.postgresql.readyAfterInit "test -e ${readyMarker} && " +
        ''pg_isready -U "''${POSTGRESQL_USERNAME:-postgres}" -d "''${POSTGRESQL_DATABASE:-postgres}" -h 127.0.0.1 -p "''${POSTGRESQL_PORT_NUMBER:-5432}"'');
    };
  };
}
