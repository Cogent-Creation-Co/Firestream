# Superset (v5) Container Options
# Copyright Firestream. MIT License.
#
# Externalized, declarative configuration for the Superset v5 container, consumed
# by bin/nix/firestream/containers/eval-container.nix. Defaults here are lifted
# VERBATIM from module.nix so that evalContainer's default build is byte-for-byte
# identical to the legacy flake.nix build path.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ lib, pkgs, ... }:

{
  config.superset = {
    version = lib.mkDefault "5";

    python = lib.mkDefault pkgs.python311;

    # Paths configuration
    # Per-key mkDefault so individual paths can be overridden independently.
    paths = {
      base = lib.mkDefault "/opt/firestream/superset";
      conf = lib.mkDefault "/opt/firestream/superset";
      data = lib.mkDefault "/opt/firestream/superset/superset_home";
      logs = lib.mkDefault "/opt/firestream/superset/superset_home/logs";
    };

    # Environment variables with defaults (preserving Bitnami compatibility)
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    env = builtins.mapAttrs (_: lib.mkDefault) {
      # Paths
      # Config + logs live UNDER the superset_home subtree so that the chart's
      # writable superset_home emptyDir covers them (chart enforces
      # readOnlyRootFilesystem; only superset_home + /tmp are writable). TMP at
      # /tmp (writable emptyDir in every pod). This makes the previous chart-side
      # extraEnvVars path-override redundant.
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
      # value wins; falls back to the historical "superset" default when unset.
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
    };

    # Variables that support Docker secrets (_FILE suffix).
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
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
    ];

    exposedPorts = lib.mkDefault [ 8088 5555 ];  # 8088=webserver, 5555=flower

    # Phase 4: enable in-image firestream-healthd. Superset exposes /health.
    # `SUPERSET_WEBSERVER_PORT_NUMBER` defaults to 8088 (see env defaults).
    health = {
      enable = lib.mkDefault true;
      readinessCmd = lib.mkDefault
        ''curl -fsS "http://localhost:''${SUPERSET_WEBSERVER_PORT_NUMBER:-8088}/health" > /dev/null'';
    };

    # Superset always needs a metadata DB and a celery broker/cache; embed
    # postgres + redis as dependency sub-services so `.#superset-up` is a
    # working out-of-the-box stack. Mirrors the airflow pattern (see
    # ../../airflow/options.nix). Whole-block mkDefault: a consumer override
    # supplies its own complete topology.
    compose = lib.mkDefault {
      projectName = "firestream-superset";
      dependencies = [ "postgresql" "redis" ];

      sharedEnv = {
        SUPERSET_DATABASE_HOST = "postgresql";
        SUPERSET_DATABASE_NAME = "superset";
        SUPERSET_DATABASE_USERNAME = "superset";
        SUPERSET_DATABASE_PASSWORD = "superset";
        REDIS_HOST = "redis";
      };

      volumes = {
        postgresql_data = { };
        redis_data = { };
      };

      services = {
        postgresql = {
          image = "@postgresql";
          env = {
            POSTGRESQL_DATABASE = "superset";
            POSTGRESQL_USERNAME = "superset";
            POSTGRESQL_PASSWORD = "superset";
            ALLOW_EMPTY_PASSWORD = "no";
          };
          ports = [ 5432 ];
          volumes = [ "postgresql_data:/firestream/postgresql" ];
          healthcheck = {
            test = [
              "CMD"
              "bash"
              "-c"
              "exec 3<>/dev/tcp/127.0.0.1/9180 && printf 'GET /readyz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200'"
            ];
            interval = "10s";
            timeout = "5s";
            retries = 5;
            start_period = "30s";
          };
        };
        redis = {
          image = "@redis";
          env.ALLOW_EMPTY_PASSWORD = "yes";
          ports = [ 6379 ];
          volumes = [ "redis_data:/firestream/redis/data" ];
          healthcheck = {
            test = [
              "CMD"
              "bash"
              "-c"
              "exec 3<>/dev/tcp/127.0.0.1/9180 && printf 'GET /readyz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200'"
            ];
            interval = "10s";
            timeout = "5s";
            retries = 5;
            start_period = "30s";
          };
        };
        superset = {
          # Own firestream-superset image (image = null inferred when omitted);
          # publish webserver + flower + healthd so the e2e harness can probe.
          ports = [ 8088 5555 9180 ];
          dependsOn = [ "postgresql" "redis" ];
        };
      };
    };
  };
}
