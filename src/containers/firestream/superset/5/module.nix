# Superset Container Module - Using Firestream Factories
# Copyright Firestream. MIT License.
#
# This module defines the Superset container using mkPythonContainerModule.
# Supports multiple roles: webserver, celery-worker, celery-beat, celery-flower, init
#
# Usage:
#   supersetModule = import ./module.nix {
#     inherit pkgs lib firestream pythonEnv supersetVersion python;
#   };

{ pkgs
, lib
, firestream
, pythonEnv          # The virtual environment from uv2nix
, supersetVersion    # e.g., "5" (major version)
, python ? pkgs.python311
, waitForPortPkg     # Rust-based port checker for init scripts

# Externalized core-surface config. Defaults below are EXACTLY today's literals
# so the legacy flake.nix path (which does not pass these) and evalContainer
# (which passes the same values from options.nix) yield identical factory args.

# Paths configuration
, paths ? {
    base = "/opt/firestream/superset";
    conf = "/opt/firestream/superset";
    data = "/opt/firestream/superset/superset_home";
    logs = "/opt/firestream/superset/superset_home/logs";
  }

# Environment variables with defaults (preserving Bitnami compatibility)
, envVars ? {
    # Paths
    # Config + logs live UNDER superset_home so the chart's writable
    # superset_home emptyDir covers them under readOnlyRootFilesystem; TMP at
    # /tmp (writable emptyDir in every pod).
    SUPERSET_HOME = "/opt/firestream/superset/superset_home";
    SUPERSET_CONFIG_PATH = "/opt/firestream/superset/superset_home/superset_config.py";
    SUPERSET_LOG_DIR = "/opt/firestream/superset/superset_home/logs";
    SUPERSET_TMP_DIR = "/tmp";
    FLASK_APP = "superset.app:create_app()";
    PYTHONPATH = "/app/pythonpath";

    # User configuration
    SUPERSET_USERNAME = "admin";
    SUPERSET_PASSWORD = "admin";
    SUPERSET_FIRSTNAME = "Superset";
    SUPERSET_LASTNAME = "Admin";
    SUPERSET_EMAIL = "admin@superset.local";

    # Webserver configuration
    SUPERSET_WEBSERVER_HOST = "0.0.0.0";
    SUPERSET_WEBSERVER_PORT_NUMBER = "8088";
    SUPERSET_WEBSERVER_WORKERS = "4";
    SUPERSET_WEBSERVER_WORKER_CLASS = "gthread";
    SUPERSET_WEBSERVER_THREADS = "20";
    SUPERSET_WEBSERVER_TIMEOUT = "60";
    SUPERSET_WEBSERVER_KEEPALIVE = "2";
    SUPERSET_WEBSERVER_MAX_REQUESTS = "0";
    SUPERSET_WEBSERVER_MAX_REQUESTS_JITTER = "0";
    SUPERSET_WEBSERVER_LIMIT_REQUEST_LINE = "0";
    SUPERSET_WEBSERVER_LIMIT_REQUEST_FIELD_SIZE = "0";
    SUPERSET_WEBSERVER_ACCESS_LOG_FILE = "-";
    SUPERSET_WEBSERVER_ERROR_LOG_FILE = "-";

    # Celery Flower configuration
    FLOWER_BASIC_AUTH = "";

    # Database configuration (metadata database)
    SUPERSET_DATABASE_DIALECT = "postgresql";
    SUPERSET_DATABASE_HOST = "postgresql";
    SUPERSET_DATABASE_PORT_NUMBER = "5432";
    SUPERSET_DATABASE_NAME = "superset";
    # The Bitnami-derived superset chart injects the metadata DB user as
    # `SUPERSET_DATABASE_USER`, while this container reads
    # `SUPERSET_DATABASE_USERNAME`. Bridge the two (mirrors the postgresql
    # POSTGRES_USER -> POSTGRESQL_USERNAME alias pattern) so the K8s-injected
    # value wins; falls back to the historical "superset" default for
    # docker-compose / standalone use where SUPERSET_DATABASE_USER is unset.
    SUPERSET_DATABASE_USERNAME = "\${SUPERSET_DATABASE_USER:-superset}";
    SUPERSET_DATABASE_PASSWORD = "";
    SUPERSET_DATABASE_USE_SSL = "no";

    # Redis configuration (cache and Celery broker)
    REDIS_HOST = "redis";
    REDIS_PORT_NUMBER = "6379";
    REDIS_USER = "";
    REDIS_PASSWORD = "";
    REDIS_CELERY_DATABASE = "0";
    REDIS_RESULTS_DATABASE = "1";
    REDIS_CACHE_DATABASE = "2";
    REDIS_USE_SSL = "no";

    # Role configuration (webserver, celery-worker, celery-beat, celery-flower, init)
    SUPERSET_ROLE = "webserver";

    # Init configuration
    SUPERSET_LOAD_EXAMPLES = "true";  # Default to loading demo data
    SUPERSET_SKIP_DATABASE_WAIT = "no";
    SUPERSET_IMPORT_DATASOURCES = "";

    # Examples database (defaults to metadata DB if empty)
    # Used by SUPERSET_LOAD_EXAMPLES to store demo data
    EXAMPLES_DATABASE_DIALECT = "";  # Empty = use SUPERSET_DATABASE_DIALECT
    EXAMPLES_DATABASE_HOST = "";     # Empty = use SUPERSET_DATABASE_HOST
    EXAMPLES_DATABASE_PORT_NUMBER = "";
    EXAMPLES_DATABASE_NAME = "";     # Empty = use SUPERSET_DATABASE_NAME
    EXAMPLES_DATABASE_USER = "";
    EXAMPLES_DATABASE_PASSWORD = "";
    EXAMPLES_DATABASE_USE_SSL = "";

    # Python cache
    PYTHONPYCACHEPREFIX = "/opt/firestream/superset/tmp/pycache";

    # Debug mode
    BITNAMI_DEBUG = "false";
  }

# Variables that support Docker secrets (_FILE suffix)
, envVarsWithSecrets ? [
    "SUPERSET_SECRET_KEY"
    "SUPERSET_USERNAME"
    "SUPERSET_PASSWORD"
    "SUPERSET_FIRSTNAME"
    "SUPERSET_LASTNAME"
    "SUPERSET_EMAIL"
    "SUPERSET_DATABASE_HOST"
    "SUPERSET_DATABASE_PORT_NUMBER"
    "SUPERSET_DATABASE_NAME"
    "SUPERSET_DATABASE_USERNAME"
    "SUPERSET_DATABASE_PASSWORD"
    "SUPERSET_DATABASE_USE_SSL"
    "REDIS_HOST"
    "REDIS_PORT_NUMBER"
    "REDIS_USER"
    "REDIS_PASSWORD"
    "REDIS_CELERY_DATABASE"
    "REDIS_RESULTS_DATABASE"
    "REDIS_CACHE_DATABASE"
    "REDIS_USE_SSL"
    "FLOWER_BASIC_AUTH"
    "SUPERSET_LOAD_EXAMPLES"
    "SUPERSET_SKIP_DATABASE_WAIT"
    "SUPERSET_IMPORT_DATASOURCES"
    "SUPERSET_ROLE"
    "SUPERSET_WEBSERVER_HOST"
    "SUPERSET_WEBSERVER_PORT_NUMBER"
    "SUPERSET_WEBSERVER_WORKERS"
    "SUPERSET_WEBSERVER_TIMEOUT"
    # Examples database (for demo data)
    "EXAMPLES_DATABASE_DIALECT"
    "EXAMPLES_DATABASE_HOST"
    "EXAMPLES_DATABASE_PORT_NUMBER"
    "EXAMPLES_DATABASE_NAME"
    "EXAMPLES_DATABASE_USER"
    "EXAMPLES_DATABASE_PASSWORD"
    "EXAMPLES_DATABASE_USE_SSL"
  ]

, exposedPorts ? [ 8088 5555 ]  # 8088=webserver, 5555=flower

# In-image health/SBOM service configuration (Phase 4). Forwarded to
# mkPythonContainerModule (which forwards to mkContainerModule). Default-off
# preserves byte-identical legacy-flake behaviour.
, health ? { enable = false; port = 9180; readinessCmd = null; }

# Image naming passthrough (parity defaults).
, imageName ? "firestream-superset"
, imageTag ? supersetVersion
}:

let
  # Read external script files
  validateScript = builtins.readFile ./scripts/validate.sh;
  configScript = builtins.readFile ./scripts/config.sh;
  initScript = builtins.readFile ./scripts/init.sh;
  secretsScript = builtins.readFile ./scripts/secrets.sh;

  # Superset-specific helper functions
  supersetHelpers = ''
    ########################
    # Generate a cryptographically secure secret key
    # Arguments:
    #   $1 - length (default: 42)
    # Returns:
    #   Random alphanumeric string suitable for Flask secrets
    #########################
    generate_secret_key() {
        local length="''${1:-42}"
        head -c 256 /dev/urandom | base64 | tr -dc 'a-zA-Z0-9' | head -c "$length"
    }

    ########################
    # URL-encode a string for use in connection strings
    # Arguments:
    #   $1 - string to encode
    # Returns:
    #   URL-encoded string
    #########################
    superset_encode_url() {
        local string="''${1:-}"
        printf '%s' "$string" | python3 -c "import sys, urllib.parse; print(urllib.parse.quote(sys.stdin.read(), safe=str()))"
    }

    ########################
    # Process SUPERSET_SECRET_KEY (required for Flask security)
    # Auto-generates if not provided
    #########################
    process_superset_secret_key() {
        if [[ -z "''${SUPERSET_SECRET_KEY:-}" ]]; then
            SUPERSET_SECRET_KEY=$(generate_secret_key 42)
            export SUPERSET_SECRET_KEY
            warn "Auto-generated SUPERSET_SECRET_KEY - set explicitly for production"
        fi
    }

    ########################
    # Execute superset CLI as appropriate user
    # Arguments:
    #   $@ - Arguments to pass to superset
    #########################
    superset_execute() {
        debug "Executing: superset $*"
        superset "$@"
    }

    ########################
    # Run Superset initialization commands
    # Performs database migrations, creates admin user, loads examples
    #########################
    superset_run_init() {
        info "Running Superset initialization..."

        # Initialize the database
        info "Applying database migrations..."
        superset_execute db upgrade

        # Initialize roles and permissions
        info "Setting up roles and permissions..."
        superset_execute init

        # Create admin user if it doesn't exist
        info "Creating admin user..."
        superset_execute fab create-admin \
            --username "''${SUPERSET_USERNAME:-admin}" \
            --firstname "''${SUPERSET_FIRSTNAME:-Superset}" \
            --lastname "''${SUPERSET_LASTNAME:-Admin}" \
            --email "''${SUPERSET_EMAIL:-admin@superset.local}" \
            --password "''${SUPERSET_PASSWORD:-admin}" || {
            debug "Admin user may already exist, continuing..."
        }

        # Load examples if requested
        if is_boolean_yes "''${SUPERSET_LOAD_EXAMPLES:-no}"; then
            info "Loading example dashboards..."
            superset_execute load_examples || warn "Failed to load some examples"
        fi

        # Import datasources if specified
        if [[ -f "''${SUPERSET_IMPORT_DATASOURCES:-}" ]]; then
            info "Importing datasources from ''${SUPERSET_IMPORT_DATASOURCES}..."
            superset_execute import_datasources -p "''${SUPERSET_IMPORT_DATASOURCES}"
        fi

        info "Superset initialization complete"
    }
  '';

  # System dependencies (libs needed in the container)
  systemDeps = with pkgs; [
    # SSL/TLS and crypto
    cacert openssl libffi
    # LDAP
    openldap cyrus_sasl
    # Database clients
    postgresql mariadb-connector-c libmysqlclient
    # Image processing (Pillow)
    libjpeg zlib libpng freetype
    # XML processing (lxml)
    libxml2 libxslt
    # Compression
    bzip2 xz zlib zstd
    # Event loop (gevent)
    libevent
    # Version control
    git openssh
    # Process management
    procps
    # Utilities
    curl wget netcat-gnu
    # Terminal
    ncurses readline
    # JSON processing
    jq
    # C++ runtime
    stdenv.cc.cc.lib
  ];

  # Runtime binary deps (need to be in PATH)
  runtimeBinDeps = with pkgs; [
    coreutils bash gnused gnugrep gawk findutils which
    postgresql git curl netcat-gnu openssh jq
    waitForPortPkg  # Rust-based port checker for init scripts
  ];

  # Superset config template with {{PLACEHOLDER}} syntax
  # Runtime activation replaces these with actual environment values
  supersetConfigTemplate = ''
    # Superset configuration template
    # Generated by Firestream Nix module
    # Placeholders are replaced at container startup via activateFn
    import os
    from datetime import timedelta

    # Flask SECRET_KEY - Required for session security
    SECRET_KEY = "{{SUPERSET_SECRET_KEY}}"

    # Database connection (metadata database)
    SQLALCHEMY_DATABASE_URI = "{{SUPERSET_DATABASE_URI}}"

    # Examples database (for demo data - defaults to metadata database)
    SQLALCHEMY_EXAMPLES_URI = "{{SUPERSET_EXAMPLES_DATABASE_URI}}"

    # Redis for caching
    CACHE_CONFIG = {
        "CACHE_TYPE": "RedisCache",
        "CACHE_DEFAULT_TIMEOUT": 300,
        "CACHE_KEY_PREFIX": "superset_",
        "CACHE_REDIS_URL": "{{SUPERSET_CACHE_REDIS_URL}}",
    }

    # Results backend for async queries
    RESULTS_BACKEND = None  # Will use database if Redis not configured

    # Celery configuration for async queries
    class CeleryConfig:
        broker_url = "{{SUPERSET_CELERY_BROKER_URL}}"
        result_backend = "{{SUPERSET_CELERY_RESULT_BACKEND}}"
        imports = (
            "superset.sql_lab",
            "superset.tasks.scheduler",
        )
        task_annotations = {
            "*": {"rate_limit": "10/s"}
        }
        beat_schedule = {
            "reports.scheduler": {
                "task": "reports.scheduler",
                "schedule": timedelta(minutes=1),
            },
        }

    CELERY_CONFIG = CeleryConfig

    # Webserver configuration
    SUPERSET_WEBSERVER_PORT = {{SUPERSET_WEBSERVER_PORT_NUMBER}}
    SUPERSET_WEBSERVER_ADDRESS = "{{SUPERSET_WEBSERVER_HOST}}"
    SUPERSET_WEBSERVER_TIMEOUT = {{SUPERSET_WEBSERVER_TIMEOUT}}

    # Feature flags
    FEATURE_FLAGS = {
        "ALERT_REPORTS": True,
        "DASHBOARD_NATIVE_FILTERS": True,
        "DASHBOARD_CROSS_FILTERS": True,
        "DASHBOARD_NATIVE_FILTERS_SET": True,
        "EMBEDDED_SUPERSET": True,
    }

    # Row limit for SQL Lab
    ROW_LIMIT = 5000
    SUPERSET_WORKERS = {{SUPERSET_WEBSERVER_WORKERS}}

    # Default authentication type (database)
    from flask_appbuilder.security.manager import AUTH_DB
    AUTH_TYPE = AUTH_DB

    # CSRF settings
    WTF_CSRF_ENABLED = True
    WTF_CSRF_EXEMPT_LIST = []
    WTF_CSRF_TIME_LIMIT = 60 * 60 * 24 * 365
  '';

in firestream.mkPythonContainerModule {
  name = "superset";
  version = supersetVersion;
  inherit pythonEnv python;

  # Paths, environment variables, and secret-aware variables are externalized
  # as function arguments (defaults equal to the historical literals). The
  # legacy flake.nix path uses the defaults; evalContainer passes the same
  # values from options.nix, yielding identical factory args.
  inherit paths envVars envVarsWithSecrets;

  # Image naming passthrough.
  inherit imageName imageTag;

  # Runtime directories with declarative schema
  runtimeDirs = {
    home = {
      path = "/opt/firestream/superset";
      type = "conf";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Superset base directory";
    };
    supersetHome = {
      path = "/opt/firestream/superset/superset_home";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Superset home directory for user data";
    };
    logs = {
      path = "/opt/firestream/superset/superset_home/logs";
      type = "logs";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Superset log files";
    };
    tmp = {
      path = "/opt/firestream/superset/tmp";
      type = "tmp";
      persistence = "ephemeral";
      mode = "1777";
      description = "Temporary files";
    };
    pycache = {
      path = "/opt/firestream/superset/tmp/pycache";
      type = "cache";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Python bytecode cache";
    };
    pythonpath = {
      path = "/app/pythonpath";
      type = "custom";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Custom Python modules directory";
    };
    bitnamiPython = {
      path = "/bitnami/python";
      type = "custom";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Bitnami Python compatibility directory";
    };
    state = {
      path = "/firestream/superset/.state";
      type = "state";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Application state tracking";
    };
  };

  # Build-time: Static config templates
  prepopulateFiles = {
    "/opt/firestream/superset/superset_config.py.template" = supersetConfigTemplate;
  };

  # Per-container helpers: emitted at top-level of libhelperssuperset.sh by the
  # engine, so chart init containers can `source /opt/firestream/scripts/libsuperset.sh`
  # and use these helpers directly.
  perContainerHelpers = supersetHelpers;

  # Validation function
  validateFn = validateScript;

  # Activation: Replace {{PLACEHOLDERS}} with runtime values
  activateFn = ''
    info "Activating Superset configuration..."

    # Process secret key first
    process_superset_secret_key

    # Build database URI
    local db_user db_pass db_ssl_opt db_uri
    db_user=$(superset_encode_url "''${SUPERSET_DATABASE_USERNAME:-superset}")
    db_pass=$(superset_encode_url "''${SUPERSET_DATABASE_PASSWORD:-}")
    db_ssl_opt=""
    is_boolean_yes "''${SUPERSET_DATABASE_USE_SSL:-no}" && db_ssl_opt="?sslmode=require"
    db_uri="''${SUPERSET_DATABASE_DIALECT:-postgresql}://''${db_user}:''${db_pass}@''${SUPERSET_DATABASE_HOST:-localhost}:''${SUPERSET_DATABASE_PORT_NUMBER:-5432}/''${SUPERSET_DATABASE_NAME:-superset}''${db_ssl_opt}"

    # Build examples database URI (defaults to metadata database)
    local ex_dialect ex_host ex_port ex_name ex_user ex_pass ex_user_enc ex_pass_enc ex_ssl_opt examples_uri
    ex_dialect="''${EXAMPLES_DATABASE_DIALECT:-''${SUPERSET_DATABASE_DIALECT:-postgresql}}"
    ex_host="''${EXAMPLES_DATABASE_HOST:-''${SUPERSET_DATABASE_HOST:-localhost}}"
    ex_port="''${EXAMPLES_DATABASE_PORT_NUMBER:-''${SUPERSET_DATABASE_PORT_NUMBER:-5432}}"
    ex_name="''${EXAMPLES_DATABASE_NAME:-''${SUPERSET_DATABASE_NAME:-superset}}"
    ex_user="''${EXAMPLES_DATABASE_USER:-''${SUPERSET_DATABASE_USERNAME:-superset}}"
    ex_pass="''${EXAMPLES_DATABASE_PASSWORD:-''${SUPERSET_DATABASE_PASSWORD:-}}"
    ex_user_enc=$(superset_encode_url "$ex_user")
    ex_pass_enc=$(superset_encode_url "$ex_pass")
    ex_ssl_opt=""
    is_boolean_yes "''${EXAMPLES_DATABASE_USE_SSL:-''${SUPERSET_DATABASE_USE_SSL:-no}}" && ex_ssl_opt="?sslmode=require"
    examples_uri="''${ex_dialect}://''${ex_user_enc}:''${ex_pass_enc}@''${ex_host}:''${ex_port}/''${ex_name}''${ex_ssl_opt}"

    # Build Redis URLs
    local redis_proto redis_pass redis_auth redis_base_url
    redis_proto="redis"
    is_boolean_yes "''${REDIS_USE_SSL:-no}" && redis_proto="rediss"
    redis_pass="''${REDIS_PASSWORD:-}"
    redis_auth=""
    if [[ -n "''${REDIS_USER:-}" ]]; then
      redis_auth="''${REDIS_USER}:''${redis_pass}@"
    elif [[ -n "$redis_pass" ]]; then
      redis_auth=":''${redis_pass}@"
    fi
    redis_base_url="''${redis_proto}://''${redis_auth}''${REDIS_HOST:-redis}:''${REDIS_PORT_NUMBER:-6379}"

    local celery_broker="''${redis_base_url}/''${REDIS_CELERY_DATABASE:-0}"
    local celery_backend="''${redis_base_url}/''${REDIS_RESULTS_DATABASE:-1}"
    local cache_url="''${redis_base_url}/''${REDIS_CACHE_DATABASE:-2}"

    # Process config template
    local config_template="/opt/firestream/superset/superset_config.py.template"
    local config_file="''${SUPERSET_CONFIG_PATH:-/opt/firestream/superset/superset_home/superset_config.py}"

    if [[ -f "$config_template" ]]; then
      ${pkgs.gnused}/bin/sed \
        -e "s|{{SUPERSET_SECRET_KEY}}|''${SUPERSET_SECRET_KEY}|g" \
        -e "s|{{SUPERSET_DATABASE_URI}}|''${db_uri}|g" \
        -e "s|{{SUPERSET_EXAMPLES_DATABASE_URI}}|''${examples_uri}|g" \
        -e "s|{{SUPERSET_CACHE_REDIS_URL}}|''${cache_url}|g" \
        -e "s|{{SUPERSET_CELERY_BROKER_URL}}|''${celery_broker}|g" \
        -e "s|{{SUPERSET_CELERY_RESULT_BACKEND}}|''${celery_backend}|g" \
        -e "s|{{SUPERSET_WEBSERVER_PORT_NUMBER}}|''${SUPERSET_WEBSERVER_PORT_NUMBER:-8088}|g" \
        -e "s|{{SUPERSET_WEBSERVER_HOST}}|''${SUPERSET_WEBSERVER_HOST:-0.0.0.0}|g" \
        -e "s|{{SUPERSET_WEBSERVER_TIMEOUT}}|''${SUPERSET_WEBSERVER_TIMEOUT:-60}|g" \
        -e "s|{{SUPERSET_WEBSERVER_WORKERS}}|''${SUPERSET_WEBSERVER_WORKERS:-4}|g" \
        "$config_template" > "$config_file"

      info "Generated superset_config.py from template"
    fi

    # Save config hash for change detection
    save_config_hash "superset" "$config_file"

    info "Superset configuration activated"
  '';

  # Initialization - from init.sh
  initFn = initScript;

  # Runtime config adjustments - from config.sh
  configFn = configScript;

  # Startup command based on role
  runCmd = ''
    ${supersetHelpers}

    case "''${SUPERSET_ROLE:-webserver}" in
      webserver)
        info "Starting Superset webserver..."
        exec gunicorn \
          --bind "''${SUPERSET_WEBSERVER_HOST:-0.0.0.0}:''${SUPERSET_WEBSERVER_PORT_NUMBER:-8088}" \
          --access-logfile "''${SUPERSET_WEBSERVER_ACCESS_LOG_FILE:--}" \
          --error-logfile "''${SUPERSET_WEBSERVER_ERROR_LOG_FILE:--}" \
          --workers "''${SUPERSET_WEBSERVER_WORKERS:-4}" \
          --worker-class "''${SUPERSET_WEBSERVER_WORKER_CLASS:-gthread}" \
          --threads "''${SUPERSET_WEBSERVER_THREADS:-20}" \
          --timeout "''${SUPERSET_WEBSERVER_TIMEOUT:-60}" \
          --keep-alive "''${SUPERSET_WEBSERVER_KEEPALIVE:-2}" \
          --max-requests "''${SUPERSET_WEBSERVER_MAX_REQUESTS:-0}" \
          --max-requests-jitter "''${SUPERSET_WEBSERVER_MAX_REQUESTS_JITTER:-0}" \
          --limit-request-line "''${SUPERSET_WEBSERVER_LIMIT_REQUEST_LINE:-0}" \
          --limit-request-field_size "''${SUPERSET_WEBSERVER_LIMIT_REQUEST_FIELD_SIZE:-0}" \
          "''${FLASK_APP}"
        ;;

      celery-worker)
        info "Starting Celery worker..."
        # `--concurrency` is REQUIRED, not a tuning nicety. Celery defaults to
        # os.cpu_count(), which inside a container reports the HOST's core count
        # and ignores the cgroup CPU limit. Each prefork child loads the whole
        # Superset app, so on a 24-core host the worker forks 24 children and is
        # OOMKilled (exit 137) against the chart's 3Gi limit — observed as an
        # endless CrashLoopBackOff that never lets the release go Ready.
        # Overridable per-deployment via the chart's `worker.extraEnvVars`.
        exec celery --app=superset.tasks.celery_app:app worker -O fair -l INFO \
          --concurrency="''${SUPERSET_CELERY_CONCURRENCY:-4}"
        ;;

      celery-beat)
        info "Starting Celery beat scheduler..."
        beat_pid="/opt/firestream/superset/tmp/superset-celerybeat.pid"
        beat_schedule="/opt/firestream/superset/tmp/superset-celerybeat-schedule"
        rm -f "$beat_pid"
        exec celery --app=superset.tasks.celery_app:app beat \
          --pidfile "$beat_pid" \
          --schedule "$beat_schedule" \
          -l INFO
        ;;

      celery-flower)
        info "Starting Celery Flower..."
        flower_args=("--app=superset.tasks.celery_app:app" "flower")
        if [[ -n "''${FLOWER_BASIC_AUTH:-}" ]]; then
          flower_args+=("--basic-auth=''${FLOWER_BASIC_AUTH}")
        fi
        exec celery "''${flower_args[@]}"
        ;;

      init)
        info "Running Superset initialization..."
        superset_run_init
        info "Initialization complete, exiting."
        exit 0
        ;;

      *)
        error "Unknown SUPERSET_ROLE: ''${SUPERSET_ROLE}"
        error "Valid roles: webserver, celery-worker, celery-beat, celery-flower, init"
        exit 1
        ;;
    esac
  '';

  inherit systemDeps runtimeBinDeps;

  inherit exposedPorts;
  inherit health;
  volumes = [ "/opt/firestream/superset/superset_home" "/opt/firestream/superset/superset_home/logs" ];

  # Python-specific options
  compileByteCode = false;  # Skip for faster builds
  requirementsPath = "/bitnami/python/requirements.txt";
  enablePip = true;

  # Development shell extras
  devShellPackages = with pkgs; [ uv docker docker-compose ];
  devShellHook = ''
    echo "Superset Version: ${supersetVersion}"
    echo ""
    echo "Build commands:"
    echo "  nix build .#dockerImage    - Build the Docker image"
    echo "  docker load < result       - Load image into Docker"
    echo ""
    echo "Test with docker-compose:"
    echo "  docker compose up -d       - Start all services"
  '';
}
