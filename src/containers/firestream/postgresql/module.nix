# PostgreSQL Container Module - Using Firestream Factories
# Copyright Firestream. MIT License.
#
# This module defines the PostgreSQL container using mkContainerModule.
# The factory handles:
# - Entrypoint generation
# - Docker image building
# - Environment loading and secrets
# - Development shell
#
# Usage:
#   postgresqlModule = import ./module.nix {
#     inherit pkgs lib firestream;
#     version = "17";  # or "16"
#   };

{ pkgs
, lib
, firestream
, version ? "17"

# Externalized core-surface config. Defaults below are EXACTLY today's literals
# so the legacy flake.nix path (which does not pass these) and evalContainer
# (which passes the same values from options.nix) yield identical factory args.

# Paths configuration (Bitnami compatibility)
, paths ? {
    base = "/opt/firestream/postgresql";
    conf = "/opt/firestream/postgresql/conf";
    data = "/firestream/postgresql/data";
    logs = "/opt/firestream/postgresql/logs";
  }

# Environment variables with defaults
# NOTE: Using pre-expanded literal values instead of shell variable references
# because Nix attribute sets are sorted alphabetically, which would cause
# dependent variables (e.g., DATA_DIR referencing VOLUME_DIR) to be exported
# before their dependencies, failing with "unbound variable" under set -u.
#
# EXCEPTION: the auth/replication/initdb defaults below intentionally read
# through the Bitnami chart's docker-style `POSTGRES_*` env names (e.g.
# `${POSTGRES_USER:-postgres}`). The Bitnami postgresql chart injects
# credentials as `POSTGRES_USER`/`POSTGRES_DATABASE`/`POSTGRES_PASSWORD[_FILE]`
# etc., but every init/helper script in this container reads the `POSTGRESQL_*`
# prefix. mkEnvDefaults emits `export VAR="${VAR:-<value>}"`, so embedding a
# `${POSTGRES_*:-default}` fallback produces the alias bridge upstream Bitnami
# ships in its `postgresql-env.sh`. Each reference is `:-`-guarded, so it is
# safe under `set -u` regardless of attribute ordering (no unbound variable).
, envVars ? {
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
    # in local/dev/e2e environments without a password override. Production
    # callers override these via the standard envVars override seam.
    #
    # Values alias the Bitnami chart's `POSTGRES_*` env names (see NOTE above);
    # the `_FILE` aliases below are exported before the secrets file-loader runs
    # so chart-mounted password files are honoured.
    POSTGRESQL_PASSWORD = "\${POSTGRES_PASSWORD:-}";
    POSTGRESQL_USERNAME = "\${POSTGRES_USER:-postgres}";
    POSTGRESQL_DATABASE = "\${POSTGRES_DATABASE:-}";
    POSTGRESQL_POSTGRES_PASSWORD = "\${POSTGRES_POSTGRES_PASSWORD:-}";
    POSTGRESQL_PASSWORD_FILE = "\${POSTGRES_PASSWORD_FILE:-}";
    POSTGRESQL_POSTGRES_PASSWORD_FILE = "\${POSTGRES_POSTGRES_PASSWORD_FILE:-}";
    POSTGRESQL_INITSCRIPTS_USERNAME = "\${POSTGRES_INITSCRIPTS_USERNAME:-}";
    POSTGRESQL_INITSCRIPTS_PASSWORD = "\${POSTGRES_INITSCRIPTS_PASSWORD:-}";
    ALLOW_EMPTY_PASSWORD = "yes";

    # Replication (values alias the chart's `POSTGRES_*` names — see NOTE above)
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
  }

# Variables supporting _FILE suffix for Docker secrets
, envVarsWithSecrets ? [
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
  ]

, exposedPorts ? [ 5432 ]

# In-image health/SBOM service configuration (Phase 3). Forwarded to
# mkContainerModule, which wraps the entrypoint with a healthd launcher when
# enabled. Default-off preserves byte-identical legacy-flake behaviour.
, health ? { enable = false; port = 9180; readinessCmd = null; }

# Image naming passthrough (parity defaults).
, imageName ? "firestream-postgresql"
, imageTag ? version
}:

let
  # Select PostgreSQL version
  postgresql = pkgs."postgresql_${version}";

  # Read external script files
  validateScript = builtins.readFile ./scripts/validate.sh;
  configScript = builtins.readFile ./scripts/config.sh;
  initScript = builtins.readFile ./scripts/init.sh;
  secretsScript = builtins.readFile ./scripts/secrets.sh;
  # Pure helper-function definitions extracted from init.sh — emitted at
  # top-level of libhelperspostgresql.sh so chart init containers can invoke
  # them after `source /opt/bitnami/scripts/libpostgresql.sh`.
  helpersInitScript = builtins.readFile ./scripts/helpers.sh;

  # PostgreSQL-specific helper functions (needed by scripts)
  # These supplement the core library functions
  postgresqlHelpers = ''
    ########################
    # Enable NSS wrapper for arbitrary UID support
    # Arguments:
    #   None
    # Returns:
    #   None
    #########################
    postgresql_enable_nss_wrapper() {
      if ! getent passwd "$(id -u)" &>/dev/null && [ -e "$NSS_WRAPPER_LIB" ]; then
        debug "Configuring libnss_wrapper..."
        export LD_PRELOAD="$NSS_WRAPPER_LIB"
        export NSS_WRAPPER_PASSWD="$(mktemp)"
        export NSS_WRAPPER_GROUP="$(mktemp)"
        echo "postgres:x:$(id -u):$(id -g):PostgreSQL:$POSTGRESQL_DATA_DIR:/bin/false" >"$NSS_WRAPPER_PASSWD"
        echo "postgres:x:$(id -g):" >"$NSS_WRAPPER_GROUP"
      fi
    }

    ########################
    # Print validation error
    # Arguments:
    #   $1 - error message
    # Returns:
    #   Sets error_code=1
    #########################
    print_validation_error() {
      error "$1"
      error_code=1
    }

    # NOTE: `replace_in_file` used to be redefined here, shadowing the shared
    # core-lib helper because that one had its parameters in the wrong order and
    # silently no-opped. The core lib is now filename-first and matches upstream
    # Bitnami (bin/nix/firestream/lib/file.nix), so this override is gone and
    # every container uses one implementation.

    ########################
    # Ensure directory exists
    # Arguments:
    #   $1 - directory path
    #########################
    ensure_dir_exists() {
      local dir="$1"
      [[ -d "$dir" ]] || mkdir -p "$dir"
    }

    ########################
    # Check if value is boolean yes
    # Arguments:
    #   $1 - value
    # Returns:
    #   0 if yes/true/1, 1 otherwise
    #########################
    is_boolean_yes() {
      local bool="''${1:-}"
      [[ "$bool" =~ ^(yes|true|1)$ ]]
    }

    ########################
    # Check if value is empty
    # Arguments:
    #   $1 - value
    # Returns:
    #   0 if empty, 1 otherwise
    #########################
    is_empty_value() {
      local value="''${1:-}"
      [[ -z "$value" ]]
    }

    ########################
    # Validate port number
    # Arguments:
    #   $1 - port
    # Returns:
    #   0 if valid, 1 otherwise
    #########################
    validate_port() {
      local port="$1"
      [[ "$port" =~ ^[0-9]+$ ]] && [[ "$port" -ge 1 ]] && [[ "$port" -le 65535 ]]
    }

    ########################
    # Wait for TCP port (wrapper around wait-for-port binary)
    # Arguments:
    #   $1 - host
    #   $2 - port
    #   $3 - timeout (optional, default 60)
    #########################
    wait_for_port() {
      local host="$1"
      local port="$2"
      local timeout="''${3:-60}"

      info "Waiting for $host:$port to be available (timeout: $timeout seconds)..."
      if wait-for-port --host "$host" --timeout "$timeout" "$port"; then
        info "$host:$port is available!"
      else
        error "Timeout waiting for $host:$port"
        return 1
      fi
    }

    ########################
    # Create postgresql.conf
    ########################
    postgresql_create_config() {
      info "Creating postgresql.conf..."
      # unix_socket_directories defaults to /run/postgresql in this build of
      # postgres, but that path lives in the image's read-only root layer.
      # When the consumer chart sets readOnlyRootFilesystem (Bitnami chart
      # default), postgres can't write its socket lock file there and dies.
      # Pin the socket dir to POSTGRESQL_TMP_DIR — chart consumers route that
      # to a writable mount; for the standalone-docker use case it stays at
      # the firestream FHS default which is owned by postgres on first boot.
      local socket_dir="''${POSTGRESQL_SOCKET_DIR:-$POSTGRESQL_TMP_DIR}"
      cat > "$POSTGRESQL_CONF_FILE" <<EOF
# PostgreSQL configuration file
# Generated by Firestream Nix-based PostgreSQL container

listen_addresses = '*'
port = $POSTGRESQL_PORT_NUMBER
unix_socket_directories = '$socket_dir'
max_connections = 100
shared_buffers = 128MB
dynamic_shared_memory_type = posix
max_wal_size = 1GB
min_wal_size = 80MB
log_timezone = 'UTC'
datestyle = 'iso, mdy'
timezone = 'UTC'
lc_messages = 'C.UTF-8'
lc_monetary = 'C.UTF-8'
lc_numeric = 'C.UTF-8'
lc_time = 'C.UTF-8'
default_text_search_config = 'pg_catalog.english'
include_dir = 'conf.d'
EOF
    }

    ########################
    # Set a property in postgresql.conf
    # Arguments:
    #   $1 - property name
    #   $2 - value
    #   $3 - config file (optional, defaults to POSTGRESQL_CONF_FILE)
    ########################
    postgresql_set_property() {
      local property="''${1:?missing property}"
      local value="''${2:?missing value}"
      local conf_file="''${3:-$POSTGRESQL_CONF_FILE}"

      if grep -qE "^#*\s*$property" "$conf_file" 2>/dev/null; then
        replace_in_file "$conf_file" "^#*\s*$property\s*=.*" "$property = '$value'"
      else
        echo "$property = '$value'" >> "$conf_file"
      fi
    }

    ########################
    # Create pg_hba.conf with password authentication
    ########################
    postgresql_password_auth_configuration() {
      info "Generating password authentication configuration..."
      cat > "$POSTGRESQL_PGHBA_FILE" <<EOF
host     all             all             0.0.0.0/0               trust
host     all             all             ::/0                    trust
EOF
    }

    ########################
    # Create pg_hba.conf with LDAP authentication
    ########################
    postgresql_ldap_auth_configuration() {
      info "Generating LDAP authentication configuration..."
      local ldap_configuration=""

      if [[ -n "$POSTGRESQL_LDAP_URL" ]]; then
        ldap_configuration="ldapurl=\"$POSTGRESQL_LDAP_URL\""
      else
        ldap_configuration="ldapserver=$POSTGRESQL_LDAP_SERVER"
        [[ -n "$POSTGRESQL_LDAP_PREFIX" ]] && ldap_configuration+=" ldapprefix=\"$POSTGRESQL_LDAP_PREFIX\""
        [[ -n "$POSTGRESQL_LDAP_SUFFIX" ]] && ldap_configuration+=" ldapsuffix=\"$POSTGRESQL_LDAP_SUFFIX\""
        [[ -n "$POSTGRESQL_LDAP_PORT" ]] && ldap_configuration+=" ldapport=$POSTGRESQL_LDAP_PORT"
        [[ -n "$POSTGRESQL_LDAP_BASE_DN" ]] && ldap_configuration+=" ldapbasedn=\"$POSTGRESQL_LDAP_BASE_DN\""
        [[ -n "$POSTGRESQL_LDAP_BIND_DN" ]] && ldap_configuration+=" ldapbinddn=\"$POSTGRESQL_LDAP_BIND_DN\""
        [[ -n "$POSTGRESQL_LDAP_BIND_PASSWORD" ]] && ldap_configuration+=" ldapbindpasswd=$POSTGRESQL_LDAP_BIND_PASSWORD"
        [[ -n "$POSTGRESQL_LDAP_SEARCH_ATTR" ]] && ldap_configuration+=" ldapsearchattribute=$POSTGRESQL_LDAP_SEARCH_ATTR"
        [[ -n "$POSTGRESQL_LDAP_SEARCH_FILTER" ]] && ldap_configuration+=" ldapsearchfilter=\"$POSTGRESQL_LDAP_SEARCH_FILTER\""
        [[ -n "$POSTGRESQL_LDAP_TLS" ]] && ldap_configuration+=" ldaptls=$POSTGRESQL_LDAP_TLS"
        [[ -n "$POSTGRESQL_LDAP_SCHEME" ]] && ldap_configuration+=" ldapscheme=$POSTGRESQL_LDAP_SCHEME"
      fi

      cat > "$POSTGRESQL_PGHBA_FILE" <<EOF
host     all             postgres        0.0.0.0/0               trust
host     all             postgres        ::/0                    trust
host     all             all             0.0.0.0/0               ldap $ldap_configuration
host     all             all             ::/0                    ldap $ldap_configuration
EOF
    }

    ########################
    # Add local connection entries to pg_hba.conf
    ########################
    postgresql_allow_local_connection() {
      cat >> "$POSTGRESQL_PGHBA_FILE" <<EOF
local    all             all                                     trust
host     all             all        127.0.0.1/32                 trust
host     all             all        ::1/128                      trust
EOF
    }

    ########################
    # Add replication entries to pg_hba.conf
    ########################
    postgresql_add_replication_to_pghba() {
      local replication_auth="trust"
      if [[ -n "$POSTGRESQL_REPLICATION_PASSWORD" ]]; then
        replication_auth="md5"
      fi
      cat >> "$POSTGRESQL_PGHBA_FILE" <<EOF
host      replication     all             0.0.0.0/0               $replication_auth
host      replication     all             ::/0                    $replication_auth
EOF
    }

    ########################
    # Restrict pg_hba.conf to use md5 instead of trust
    ########################
    postgresql_restrict_pghba() {
      if [[ -n "$POSTGRESQL_PASSWORD" ]]; then
        replace_in_file "$POSTGRESQL_PGHBA_FILE" "trust" "md5"
      fi
    }

    ########################
    # Create pg_hba.conf based on configuration
    ########################
    postgresql_create_pghba() {
      info "Creating pg_hba.conf..."
      if is_boolean_yes "$POSTGRESQL_ENABLE_LDAP"; then
        postgresql_ldap_auth_configuration
      else
        postgresql_password_auth_configuration
      fi
      postgresql_allow_local_connection
    }

    ########################
    # Configure replication parameters in postgresql.conf
    ########################
    postgresql_configure_replication_parameters() {
      info "Configuring replication parameters..."
      postgresql_set_property "wal_level" "$POSTGRESQL_WAL_LEVEL"
      postgresql_set_property "max_wal_size" "400MB"
      postgresql_set_property "max_wal_senders" "16"
      postgresql_set_property "wal_keep_size" "128MB"
      postgresql_set_property "hot_standby" "on"

      if is_boolean_yes "$POSTGRESQL_REPLICATION_USE_PASSFILE" && [[ ! -f "$POSTGRESQL_REPLICATION_PASSFILE_PATH" ]]; then
        echo "*:*:*:$POSTGRESQL_REPLICATION_USER:$POSTGRESQL_REPLICATION_PASSWORD" > "$POSTGRESQL_REPLICATION_PASSFILE_PATH"
        chmod 600 "$POSTGRESQL_REPLICATION_PASSFILE_PATH"
      fi
    }

    ########################
    # Configure synchronous replication
    ########################
    postgresql_configure_synchronous_replication() {
      local replication_nodes=""
      local synchronous_standby_names=""
      info "Configuring synchronous replication..."

      replication_nodes="\"$POSTGRESQL_CLUSTER_APP_NAME\""

      if [[ ''${POSTGRESQL_NUM_SYNCHRONOUS_REPLICAS:-0} -gt 0 ]]; then
        synchronous_standby_names="$POSTGRESQL_NUM_SYNCHRONOUS_REPLICAS ($replication_nodes)"
        postgresql_set_property "synchronous_commit" "$POSTGRESQL_SYNCHRONOUS_COMMIT_MODE"
        postgresql_set_property "synchronous_standby_names" "$synchronous_standby_names"
      fi
    }

    ########################
    # Configure TLS
    ########################
    postgresql_configure_tls() {
      info "Configuring TLS..."
      chmod 600 "$POSTGRESQL_TLS_KEY_FILE" || warn "Could not set permissions on TLS key file"
      postgresql_set_property "ssl" "on"
      ! is_boolean_yes "$POSTGRESQL_TLS_PREFER_SERVER_CIPHERS" && postgresql_set_property "ssl_prefer_server_ciphers" "off"
      [[ -n "$POSTGRESQL_TLS_CA_FILE" ]] && postgresql_set_property "ssl_ca_file" "$POSTGRESQL_TLS_CA_FILE"
      [[ -n "$POSTGRESQL_TLS_CRL_FILE" ]] && postgresql_set_property "ssl_crl_file" "$POSTGRESQL_TLS_CRL_FILE"
      postgresql_set_property "ssl_cert_file" "$POSTGRESQL_TLS_CERT_FILE"
      postgresql_set_property "ssl_key_file" "$POSTGRESQL_TLS_KEY_FILE"
    }

    ########################
    # Configure fsync
    ########################
    postgresql_configure_fsync() {
      info "Configuring fsync..."
      postgresql_set_property "fsync" "$POSTGRESQL_FSYNC"
    }

    ########################
    # Configure recovery for slave mode
    ########################
    postgresql_configure_recovery() {
      info "Setting up streaming replication slave..."
      local conninfo_password
      if is_boolean_yes "$POSTGRESQL_REPLICATION_USE_PASSFILE"; then
        conninfo_password="passfile=$POSTGRESQL_REPLICATION_PASSFILE_PATH"
      else
        conninfo_password="password=''${POSTGRESQL_REPLICATION_PASSWORD//\&/\\&}"
      fi
      postgresql_set_property "primary_conninfo" "host=$POSTGRESQL_MASTER_HOST port=$POSTGRESQL_MASTER_PORT_NUMBER user=$POSTGRESQL_REPLICATION_USER $conninfo_password application_name=$POSTGRESQL_CLUSTER_APP_NAME"
      touch "$POSTGRESQL_DATA_DIR/standby.signal"
    }

    # ----------------------------------------------------------------------
    # Helpers relocated from scripts/init.sh so they're visible at top-level
    # of libpostgresql.sh (chart init containers source the lib and call
    # postgresql_execute, postgresql_initialize, etc. directly).
    # Bodies are unchanged from the original scripts/init.sh definitions.
    # ----------------------------------------------------------------------
    ${helpersInitScript}
  '';

  # System dependencies (libs needed in the container)
  systemDeps = with pkgs; [
    # SSL/TLS
    cacert openssl

    # LDAP support
    openldap cyrus_sasl

    # Compression
    bzip2 xz zlib zstd lz4

    # XML support
    libxml2

    # Utilities
    coreutils gnugrep gnused gawk findutils which curl netcat-gnu gzip
    procps util-linux

    # Terminal
    ncurses readline

    # NSS wrapper for random UID support
    nss_wrapper
  ];

  # Runtime binary deps (need to be in PATH)
  runtimeBinDeps = with pkgs; [
    coreutils bash gnused gnugrep gawk findutils which
    postgresql gzip netcat-gnu
    # S3 client for the pg_dumpall backup CronJob (reuses the server image) —
    # streams logical dumps straight into SeaweedFS via `aws s3 cp -`.
    awscli2
  ];

in firestream.mkContainerModule {
  name = "postgresql";
  inherit version;

  # Paths, environment variables, and secret-aware variables are externalized
  # as function arguments (defaults equal to the historical literals). The
  # legacy flake.nix path uses the defaults; evalContainer passes the same
  # values from options.nix, yielding identical factory args.
  inherit paths envVars envVarsWithSecrets;

  # Image naming passthrough.
  inherit imageName imageTag;

  # Declarative directory schema
  runtimeDirs = {
    data = {
      path = "/firestream/postgresql/data";
      type = "data";
      persistence = "persistent";
      mode = "0700";
      owner = 1001;
      group = 1001;
      description = "PostgreSQL data directory";
    };
    conf = {
      path = "/opt/firestream/postgresql/conf";
      type = "conf";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "PostgreSQL configuration directory";
    };
    confDefault = {
      path = "/opt/firestream/postgresql/conf.default";
      type = "conf";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Default configuration templates";
    };
    confD = {
      path = "/opt/firestream/postgresql/conf/conf.d";
      type = "conf";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Additional configuration directory";
    };
    logs = {
      path = "/opt/firestream/postgresql/logs";
      type = "logs";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "PostgreSQL log directory";
    };
    tmp = {
      path = "/opt/firestream/postgresql/tmp";
      type = "tmp";
      persistence = "ephemeral";
      mode = "1777";
      description = "Temporary files";
    };
    run = {
      path = "/run/postgresql";
      type = "tmp";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Runtime socket directory";
    };
    volumeDir = {
      path = "/firestream/postgresql";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Persistent volume root";
    };
    preinitdb = {
      path = "/docker-entrypoint-preinitdb.d";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Pre-initialization scripts";
    };
    initdb = {
      path = "/docker-entrypoint-initdb.d";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Initialization scripts";
    };
  };

  # Per-container helpers: emitted at top-level of libhelperspostgresql.sh by the
  # engine, so chart init containers can `source /opt/bitnami/scripts/libpostgresql.sh`
  # and use these helpers directly.
  perContainerHelpers = postgresqlHelpers;

  # Validation function
  validateFn = ''
    error_code=0
    ${validateScript}
    [[ "$error_code" -eq 0 ]] || exit "$error_code"
  '';

  # Activation: Load secrets and enable NSS wrapper
  activateFn = ''
    info "Activating PostgreSQL configuration..."

    # Enable NSS wrapper for arbitrary UID support
    postgresql_enable_nss_wrapper

    # Load secrets from _FILE variables
    ${secretsScript}

    info "PostgreSQL configuration activated"
  '';

  # Configuration generation
  configFn = configScript;

  # Initialization (database setup, users, replication)
  initFn = initScript;

  # Startup command
  runCmd = ''
    ${postgresqlHelpers}

    # Enable NSS wrapper
    postgresql_enable_nss_wrapper

    info "Starting PostgreSQL..."
    exec postgres \
      -D "$POSTGRESQL_DATA_DIR" \
      --config-file="$POSTGRESQL_CONF_FILE" \
      --hba_file="$POSTGRESQL_PGHBA_FILE"
  '';

  inherit systemDeps runtimeBinDeps;

  inherit exposedPorts;
  inherit health;
  volumes = [ "/firestream/postgresql" "/docker-entrypoint-initdb.d" "/docker-entrypoint-preinitdb.d" ];

  user = { name = "postgres"; group = "postgres"; uid = 1001; gid = 1001; };

  # Development shell extras
  devShellPackages = with pkgs; [ docker docker-compose ];
  devShellHook = ''
    echo "PostgreSQL Version: ${version}"
    echo "PostgreSQL Binary: ${postgresql}/bin/postgres"
    echo ""
    echo "Build commands:"
    echo "  nix build .#dockerImage    - Build the Docker image"
    echo "  docker load < result       - Load image into Docker"
  '';
}
