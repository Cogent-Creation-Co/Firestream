# Kafka Container Module - Using Firestream Java Factory
# Copyright Firestream. MIT License.
#
# This module defines the Kafka container using mkJavaContainerModule.
# The Java factory provides automatic JDK configuration, JVM tuning options,
# NSS wrapper support for Kubernetes, and classpath management.
#
# Note: This module is for Kafka 4.0+ (KRaft mode only, no ZooKeeper)
#
# Usage:
#   kafkaModule = import ./module.nix {
#     inherit pkgs lib firestream;
#     version = "4.0";
#   };

{ pkgs
, lib
, firestream
, version ? "4.0"

# Externalized core-surface config. Defaults below are EXACTLY today's literals
# so the legacy flake.nix path (which does not pass these) and evalContainer
# (which passes the same values from options.nix) yield identical factory args.
# NOTE: mkJavaContainerModule internally merges its javaEnvVars (heap/gc) with
# the envVars below; this arg carries exactly the value the module passed before.

# Paths configuration (Bitnami compatibility)
, paths ? {
    base = "/opt/firestream/kafka";
    conf = "/opt/firestream/kafka/config";
    data = "/firestream/kafka/data";
    logs = "/opt/firestream/kafka/logs";
  }

# Environment variables with defaults
# NOTE: Use absolute paths, NOT variable references like \${VAR} - bash strict mode (set -u) fails on those
, envVars ? {
    # Base directories
    KAFKA_BASE_DIR = "/opt/firestream/kafka";
    KAFKA_VOLUME_DIR = "/firestream/kafka";
    KAFKA_DATA_DIR = "/firestream/kafka/data";
    KAFKA_CONF_DIR = "/opt/firestream/kafka/config";
    KAFKA_MOUNTED_CONF_DIR = "/firestream/kafka/config";
    KAFKA_CONF_FILE = "/opt/firestream/kafka/config/server.properties";
    KAFKA_LOG_DIR = "/opt/firestream/kafka/logs";
    KAFKA_HOME = "/opt/firestream/kafka";
    KAFKA_CERTS_DIR = "/opt/firestream/kafka/config/certs";
    KAFKA_TMP_DIR = "/opt/firestream/kafka/tmp";
    KAFKA_PID_FILE = "/opt/firestream/kafka/tmp/kafka.pid";
    KAFKA_INITSCRIPTS_DIR = "/docker-entrypoint-initdb.d";

    # User and group
    KAFKA_DAEMON_USER = "kafka";
    KAFKA_DAEMON_GROUP = "kafka";

    # Default ports
    KAFKA_PORT_NUMBER = "9092";
    KAFKA_CONTROLLER_PORT_NUMBER = "9093";

    # KRaft configuration (required for Kafka 4.0+)
    # Defaults for single-node combined-mode (broker + controller) so the
    # bare image runs out of the box for local/dev/e2e. Production / multi-node
    # callers override these via the standard envVars override seam.
    KAFKA_CFG_NODE_ID = "0";
    KAFKA_CFG_PROCESS_ROLES = "controller,broker";
    KAFKA_CFG_CONTROLLER_QUORUM_VOTERS = "0@localhost:9093";
    KAFKA_CFG_CONTROLLER_LISTENER_NAMES = "CONTROLLER";
    KAFKA_CLUSTER_ID = "";
    KAFKA_INITIAL_CONTROLLERS = "";
    KAFKA_KRAFT_BOOTSTRAP_SCRAM_USERS = "false";

    # Listeners — PLAINTEXT for client + CONTROLLER for KRaft on the standard ports.
    KAFKA_CFG_LISTENERS = "PLAINTEXT://:9092,CONTROLLER://:9093";
    KAFKA_CFG_ADVERTISED_LISTENERS = "PLAINTEXT://localhost:9092";
    KAFKA_CFG_LISTENER_SECURITY_PROTOCOL_MAP = "CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT";
    KAFKA_CFG_INTER_BROKER_LISTENER_NAME = "PLAINTEXT";
    KAFKA_CLIENT_LISTENER_NAME = "CLIENT";

    # SASL configuration
    KAFKA_CFG_SASL_ENABLED_MECHANISMS = "";
    KAFKA_CFG_SASL_MECHANISM_INTER_BROKER_PROTOCOL = "";
    KAFKA_CFG_SASL_MECHANISM_CONTROLLER_PROTOCOL = "";

    # SASL users and passwords (empty by default for plaintext mode)
    KAFKA_CLIENT_USERS = "";
    KAFKA_CLIENT_PASSWORDS = "";
    KAFKA_INTER_BROKER_USER = "";
    KAFKA_INTER_BROKER_PASSWORD = "";
    KAFKA_CONTROLLER_USER = "";
    KAFKA_CONTROLLER_PASSWORD = "";
    KAFKA_CLIENT_SASL_MECHANISM = "";

    # TLS/SSL configuration
    KAFKA_TLS_TYPE = "JKS";
    KAFKA_TLS_CLIENT_AUTH = "none";
    KAFKA_TLS_TRUSTSTORE_FILE = "";
    KAFKA_CERTIFICATE_PASSWORD = "";
    KAFKA_KEYSTORE_PASSWORD = "";
    KAFKA_TRUSTSTORE_PASSWORD = "";
    KAFKA_KEY_PASSWORD = "";

    # Dynamic environment variable commands
    KAFKA_NODE_ID_COMMAND = "";
    KAFKA_CONTROLLER_QUORUM_VOTERS_COMMAND = "";

    # Other settings
    KAFKA_INIT_MAX_TIMEOUT = "60";
    KAFKA_CFG_LOG_DIRS = "/firestream/kafka/data";
    KAFKA_CFG_MAX_REQUEST_SIZE = "";
    KAFKA_CFG_MAX_PARTITION_FETCH_BYTES = "";

    # Note: JAVA_HOME and NSS_WRAPPER_LIB are auto-set by mkJavaContainerModule

    # Debug mode
    BITNAMI_DEBUG = "false";

    # Plaintext listener warning control
    ALLOW_PLAINTEXT_LISTENER = "no";
  }

# Variables supporting _FILE suffix for Docker secrets
, envVarsWithSecrets ? [
    "KAFKA_CLIENT_PASSWORDS"
    "KAFKA_INTER_BROKER_PASSWORD"
    "KAFKA_CONTROLLER_PASSWORD"
    "KAFKA_CERTIFICATE_PASSWORD"
    "KAFKA_KEYSTORE_PASSWORD"
    "KAFKA_TRUSTSTORE_PASSWORD"
    "KAFKA_KEY_PASSWORD"
    "KAFKA_CLIENT_USERS"
    "KAFKA_INTER_BROKER_USER"
    "KAFKA_CONTROLLER_USER"
    "KAFKA_CLUSTER_ID"
  ]

, exposedPorts ? [ 9092 9093 ]

# In-image health/SBOM service configuration (Phase 4). Forwarded to
# mkJavaContainerModule (which forwards to mkContainerModule). Default-off
# preserves byte-identical legacy-flake behaviour.
, health ? { enable = false; port = 9180; readinessCmd = null; }

# Image naming passthrough (parity defaults).
, imageName ? "firestream-kafka"
, imageTag ? version
}:

let
  # Use apacheKafka from nixpkgs
  kafka = pkgs.apacheKafka;

  # Java runtime for Kafka
  javaRuntime = pkgs.temurin-bin-17;

  # Read external script files
  validateScript = builtins.readFile ./scripts/validate.sh;
  configScript = builtins.readFile ./scripts/config.sh;
  initScript = builtins.readFile ./scripts/init.sh;
  secretsScript = builtins.readFile ./scripts/secrets.sh;
  # Pure helper-function definitions extracted from {validate,config,init}.sh —
  # emitted at top-level of libhelperskafka.sh so chart init containers can
  # invoke kafka_server_conf_set, kafka_common_conf_set, etc. after
  # `source /opt/bitnami/scripts/libkafka.sh`.
  helpersScript = builtins.readFile ./scripts/helpers.sh;

  # Kafka-specific helper functions (needed by scripts)
  # These supplement the core library functions
  # Note: NSS wrapper is provided by mkJavaContainerModule via java_enable_nss_wrapper()
  kafkaHelpers = ''
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
    # Execute command with debug output if BITNAMI_DEBUG is enabled
    # Arguments:
    #   $@ - command and arguments
    #########################
    debug_execute() {
      if is_boolean_yes "''${BITNAMI_DEBUG:-false}"; then
        "$@"
      else
        "$@" >/dev/null 2>&1
      fi
    }

    ########################
    # Get PID from file
    # Arguments:
    #   $1 - PID file path
    # Returns:
    #   PID value or empty string
    #########################
    get_pid_from_file() {
      local pid_file="$1"
      if [[ -f "$pid_file" ]]; then
        cat "$pid_file"
      fi
    }

    ########################
    # Check if a service is running
    # Arguments:
    #   $1 - PID
    # Returns:
    #   0 if running, 1 otherwise
    #########################
    is_service_running() {
      local pid="$1"
      if [[ -n "$pid" ]]; then
        kill -0 "$pid" 2>/dev/null
      else
        return 1
      fi
    }

    ########################
    # Stop a service using its PID file
    # Arguments:
    #   $1 - PID file path
    #   $2 - Signal (default: TERM)
    #########################
    stop_service_using_pid() {
      local pid_file="$1"
      local signal="''${2:-TERM}"
      local pid

      pid="$(get_pid_from_file "$pid_file")"
      if [[ -n "$pid" ]]; then
        info "Stopping process with PID $pid..."
        kill -"$signal" "$pid" 2>/dev/null
        # Wait for process to terminate
        local timeout=30
        local elapsed=0
        while kill -0 "$pid" 2>/dev/null; do
          if [[ $elapsed -ge $timeout ]]; then
            warn "Process $pid did not terminate gracefully, sending SIGKILL"
            kill -9 "$pid" 2>/dev/null
            break
          fi
          sleep 1
          ((elapsed++))
        done
        rm -f "$pid_file"
      fi
    }

    ########################
    # Check if running as root
    # Returns:
    #   0 if root, 1 otherwise
    #########################
    am_i_root() {
      [[ "$(id -u)" -eq 0 ]]
    }

    ########################
    # Run command as a specific user
    # Arguments:
    #   $1 - user
    #   $@ - command and arguments
    #########################
    run_as_user() {
      local user="$1"
      shift
      if am_i_root; then
        su -s /bin/bash "$user" -c "$*"
      else
        "$@"
      fi
    }

    # ----------------------------------------------------------------------
    # Helpers relocated from scripts/{validate,config,init}.sh so they're
    # visible at top-level of libkafka.sh (chart init containers source the
    # lib and call kafka_server_conf_set, kafka_common_conf_set, etc. directly).
    # Bodies are unchanged from the original script definitions.
    # ----------------------------------------------------------------------
    ${helpersScript}
  '';

  # System dependencies (Kafka-specific extras)
  # Note: Java deps (cacert, openssl, zlib, procps, nss_wrapper, etc.) are auto-included
  # by mkJavaContainerModule
  systemDeps = with pkgs; [
    # Compression
    gzip bzip2

    # Utilities
    coreutils gnugrep gnused gawk findutils which curl netcat-gnu
    util-linux

    # Terminal
    ncurses readline
  ];

  # Runtime binary deps (need to be in PATH)
  # Note: jdk is auto-included by mkJavaContainerModule
  runtimeBinDeps = with pkgs; [
    coreutils bash gnused gnugrep gawk findutils which
    kafka  # apacheKafka
    gzip netcat-gnu
  ];

in firestream.mkJavaContainerModule {
  name = "kafka";
  inherit version;

  # Java configuration - use javaRuntime (Temurin 17)
  jdk = javaRuntime;

  # JVM tuning for Kafka workloads
  heapOpts = "-Xmx1g -Xms512m";
  gcOpts = "-XX:+UseG1GC -XX:MaxGCPauseMillis=20 -XX:InitiatingHeapOccupancyPercent=35";

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
      path = "/firestream/kafka/data";
      type = "data";
      persistence = "persistent";
      mode = "0700";
      owner = 1001;
      group = 1001;
      description = "Kafka data directory (log.dirs)";
    };
    conf = {
      path = "/opt/firestream/kafka/config";
      type = "conf";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Kafka configuration directory";
    };
    confDefault = {
      path = "/opt/firestream/kafka/config.default";
      type = "conf";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Default configuration templates";
    };
    logs = {
      path = "/opt/firestream/kafka/logs";
      type = "logs";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Kafka log directory";
    };
    tmp = {
      path = "/opt/firestream/kafka/tmp";
      type = "tmp";
      persistence = "ephemeral";
      mode = "1777";
      description = "Temporary files and PID file";
    };
    certs = {
      path = "/opt/firestream/kafka/config/certs";
      type = "conf";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "TLS certificates directory";
    };
    volumeDir = {
      path = "/firestream/kafka";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Persistent volume root";
    };
    mountedConf = {
      path = "/firestream/kafka/config";
      type = "conf";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "User-mounted configuration directory";
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
    systemTmp = {
      path = "/tmp";
      type = "tmp";
      persistence = "ephemeral";
      mode = "1777";
      description = "System temporary directory";
    };
  };

  # Per-container helpers: emitted at top-level of libhelperskafka.sh by the
  # engine, so chart init containers can `source /opt/bitnami/scripts/libkafka.sh`
  # and use these helpers (kafka_server_conf_set, etc.) directly.
  perContainerHelpers = kafkaHelpers;

  # Validation function
  validateFn = ''
    error_code=0
    ${validateScript}
    [[ "$error_code" -eq 0 ]] || exit "$error_code"
  '';

  # Activation: Load secrets
  # Note: NSS wrapper is automatically enabled by mkJavaContainerModule
  activateFn = ''
    info "Activating Kafka configuration..."

    # Load secrets from _FILE variables
    ${secretsScript}

    info "Kafka configuration activated"
  '';

  # Configuration generation
  configFn = configScript;

  # Initialization (KRaft storage setup, directory creation)
  # Include configScript for helper functions like kafka_configure_default_truststore_locations
  initFn = configScript + initScript;

  # Startup command
  # Note: NSS wrapper is automatically enabled by mkJavaContainerModule before runCmd
  runCmd = ''
    ${kafkaHelpers}

    info "Starting Kafka..."
    exec ${kafka}/bin/kafka-server-start.sh "$KAFKA_CONF_FILE"
  '';

  inherit systemDeps runtimeBinDeps;

  inherit exposedPorts;
  inherit health;
  volumes = [ "/firestream/kafka" "/docker-entrypoint-initdb.d" ];

  user = { name = "kafka"; group = "kafka"; uid = 1001; gid = 1001; };

  # Development shell extras
  devShellPackages = with pkgs; [ docker docker-compose ];
  devShellHook = ''
    echo "Kafka Version: ${version}"
    echo "Kafka Binary: ${kafka}/bin/kafka-server-start.sh"
    echo "Java Home: ${javaRuntime}"
    echo ""
    echo "Build commands:"
    echo "  nix build .#dockerImage    - Build the Docker image"
    echo "  docker load < result       - Load image into Docker"
    echo ""
    echo "Note: This module is for Kafka 4.0+ (KRaft mode only, no ZooKeeper)"
  '';
}
