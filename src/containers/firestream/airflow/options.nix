# Airflow Container Options
# Copyright Firestream. MIT License.
#
# Externalized, declarative configuration for the Airflow container, consumed
# by bin/nix/firestream/containers/eval-container.nix. Defaults here are lifted
# VERBATIM from module.nix so that evalContainer's default build is byte-for-byte
# identical to the legacy flake.nix build path.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.
# Per-leaf mkDefault lets a consumer override a single env var while every other
# default is preserved.

{ lib, pkgs, config, ... }:

let
  # Spec for one vendored DAG source. `src` (a derivation/path), when set, WINS
  # over owner/repo/rev/hash so a local `dags/` folder or any non-GitHub source
  # works too. See ./vendor-dags.nix for how these become a baked
  # /opt/firestream/airflow/dags tree.
  vendoredDagSpec = lib.types.submodule {
    options = {
      name = lib.mkOption {
        type = lib.types.str;
        description = "Label for this source (used in build logs and collision errors).";
      };
      owner = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "GitHub owner (when fetching via fetchFromGitHub).";
      };
      repo = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "GitHub repo (when fetching via fetchFromGitHub).";
      };
      rev = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Commit or tag to pin (when fetching via fetchFromGitHub).";
      };
      hash = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "fetchFromGitHub sha256 (SRI string).";
      };
      src = lib.mkOption {
        type = lib.types.nullOr (lib.types.either lib.types.path lib.types.package);
        default = null;
        description = ''
          A prebuilt source tree (derivation or path). When set, it WINS over
          owner/repo/rev/hash, enabling a local `dags/` folder, fetchgit, or a
          flake input. This is the common "bake my DAG" case.
        '';
      };
      sourceRoot = lib.mkOption {
        type = lib.types.str;
        default = ".";
        description = "Subdirectory inside the source that contains the DAG files.";
      };
    };
  };
in
{
  # Build-time DAG vendoring: a list of DAG-source specs baked into the image at
  # /opt/firestream/airflow/dags (Airflow's dags_folder). Empty by default —
  # stock images are unchanged. Forwarded to the container factory via
  # `extraModuleArgs.vendoredDags` below (the eval-container.nix seam).
  options.airflow.vendoredDags = lib.mkOption {
    type = lib.types.listOf vendoredDagSpec;
    default = [ ];
    description = ''
      DAG sources to vendor at build time. Each entry is fetched (GitHub by
      default, or any `src`), and its files are laid into a baked
      /opt/firestream/airflow/dags directory the scheduler scans directly.
    '';
  };

  # Opt-in guest-DAG Python dependency workspace. When set, its uv2nix
  # workspace (pyproject.toml + uv.lock, + optional overrides.nix) is resolved
  # into a SEPARATE venv baked at /opt/firestream/airflow/dags-venv, isolated
  # from Airflow's own venv. null (default) => stock image byte-for-byte
  # unchanged — same opt-in discipline as vendoredDags above. Lowered into the
  # generic `extraWorkspaces` option below (the eval-container.nix seam).
  options.airflow.dagWorkspace = lib.mkOption {
    type = lib.types.nullOr (lib.types.submodule {
      options = {
        src = lib.mkOption {
          type = lib.types.path;
          description = "Directory containing the guest DAG pyproject.toml + uv.lock (+ optional overrides.nix). Resolved via uv2nix into a separate venv baked at /opt/firestream/airflow/dags-venv, isolated from Airflow's own venv.";
        };
        overrides = lib.mkOption {
          type = lib.types.nullOr lib.types.path;
          default = null;
          description = "Optional path to an overrides.nix for the guest venv's C-extension packages.";
        };
      };
    });
    default = null;
    description = "Opt-in: declare guest-DAG Python dependencies as their own uv2nix workspace. null (default) => stock image unchanged.";
  };

  config.airflow = {
    # Forward the vendored-dags list to module.nix through the factory's
    # extraModuleArgs seam (eval-container.nix splices this into moduleArgs).
    extraModuleArgs.vendoredDags = config.airflow.vendoredDags;

    # Lower the opt-in dagWorkspace into the generic extraWorkspaces option
    # (declared in eval-container.nix). One entry named "dags" => a separate
    # venv baked at /opt/firestream/airflow/dags-venv. `python` is omitted so
    # eval-container's resolver defaults it to cfg.python.
    extraWorkspaces = lib.optional (config.airflow.dagWorkspace != null) {
      name = "dags";
      inherit (config.airflow.dagWorkspace) src overrides;
    };

    version = lib.mkDefault "3.0.3";

    python = lib.mkDefault pkgs.python312;

    # Paths configuration (Airflow uses /opt/firestream/airflow structure)
    # Per-key mkDefault so individual paths can be overridden independently.
    paths = {
      base = lib.mkDefault "/opt/firestream/airflow";
      conf = lib.mkDefault "/opt/firestream/airflow";
      data = lib.mkDefault "/opt/firestream/airflow/dags";
      logs = lib.mkDefault "/opt/firestream/airflow/logs";
    };

    # Environment variables with defaults (from env-defaults.sh)
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    env = builtins.mapAttrs (_: lib.mkDefault) ({
      # Paths
      AIRFLOW_HOME = "/opt/firestream/airflow";
      AIRFLOW_DAGS_DIR = "/opt/firestream/airflow/dags";
      AIRFLOW_LOGS_DIR = "/opt/firestream/airflow/logs";
      AIRFLOW_SCHEDULER_LOGS_DIR = "/opt/firestream/airflow/logs/scheduler";
      AIRFLOW_PLUGINS_DIR = "/opt/firestream/airflow/plugins";
      AIRFLOW_TMP_DIR = "/opt/firestream/airflow/tmp";
      AIRFLOW_CONF_FILE = "/opt/firestream/airflow/airflow.cfg";
      AIRFLOW_WEBSERVER_CONF_FILE = "/opt/firestream/airflow/webserver_config.py";

      # User configuration
      AIRFLOW_USERNAME = "admin";
      AIRFLOW_PASSWORD = "admin";
      AIRFLOW_FIRSTNAME = "Firstname";
      AIRFLOW_LASTNAME = "Lastname";
      AIRFLOW_EMAIL = "user@example.com";

      # Component configuration
      AIRFLOW_COMPONENT_TYPE = "api-server";
      AIRFLOW_EXECUTOR = "LocalExecutor";
      AIRFLOW_SKIP_DB_SETUP = "no";
      AIRFLOW_LOAD_EXAMPLES = "yes";
      AIRFLOW_STANDALONE_DAG_PROCESSOR = "no";
      AIRFLOW_DB_MIGRATE_TIMEOUT = "120";
      AIRFLOW_FORCE_OVERWRITE_CONF_FILE = "no";

      # API Server / Webserver configuration
      AIRFLOW_APISERVER_HOST = "127.0.0.1";
      AIRFLOW_APISERVER_PORT_NUMBER = "8080";
      AIRFLOW_WEBSERVER_SECRET_KEY = "airflow-web-server-key";
      AIRFLOW_APISERVER_SECRET_KEY = "airflow-api-server-key";
      AIRFLOW_JWT_SECRET_KEY = "";  # Auto-generated if empty
      AIRFLOW_ENABLE_HTTPS = "no";
      AIRFLOW_EXTERNAL_APISERVER_PORT_NUMBER = "80";

      # Triggerer configuration
      AIRFLOW_TRIGGERER_DEFAULT_CAPACITY = "1000";

      # Database configuration
      AIRFLOW_DATABASE_HOST = "postgresql";
      AIRFLOW_DATABASE_PORT_NUMBER = "5432";
      AIRFLOW_DATABASE_NAME = "airflow";
      AIRFLOW_DATABASE_USERNAME = "airflow";
      AIRFLOW_DATABASE_USE_SSL = "no";
      AIRFLOW_DATABASE_PASSWORD = "";

      # Redis (Celery) configuration
      REDIS_HOST = "redis";
      REDIS_PORT_NUMBER = "6379";
      REDIS_DATABASE = "1";
      REDIS_USER = "";
      REDIS_PASSWORD = "";
      AIRFLOW_REDIS_USE_SSL = "no";

      # LDAP configuration
      AIRFLOW_LDAP_ENABLE = "no";
      AIRFLOW_LDAP_USER_REGISTRATION = "True";
      AIRFLOW_LDAP_ROLES_SYNC_AT_LOGIN = "True";
      AIRFLOW_LDAP_USE_TLS = "False";
      AIRFLOW_LDAP_ALLOW_SELF_SIGNED = "True";

      # Python cache
      PYTHONPYCACHEPREFIX = "/opt/firestream/airflow/tmp/pycache";

      # Debug mode
      BITNAMI_DEBUG = "false";
    } // lib.optionalAttrs (config.airflow.dagWorkspace != null) {
      # Guest-DAG venv (baked only when dagWorkspace is opted in). Points DAG
      # code at its own isolated interpreter, separate from Airflow's venv.
      FIRESTREAM_DAGS_VENV = "/opt/firestream/airflow/dags-venv";
      FIRESTREAM_DAGS_PYTHON = "/opt/firestream/airflow/dags-venv/bin/python";
    });

    # Variables that support Docker secrets (_FILE suffix).
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
      "AIRFLOW_USERNAME"
      "AIRFLOW_PASSWORD"
      "AIRFLOW_FIRSTNAME"
      "AIRFLOW_LASTNAME"
      "AIRFLOW_EMAIL"
      "AIRFLOW_COMPONENT_TYPE"
      "AIRFLOW_EXECUTOR"
      "AIRFLOW_RAW_FERNET_KEY"
      "AIRFLOW_FORCE_OVERWRITE_CONF_FILE"
      "AIRFLOW_FERNET_KEY"
      "AIRFLOW_WEBSERVER_SECRET_KEY"
      "AIRFLOW_APISERVER_SECRET_KEY"
      "AIRFLOW_JWT_SECRET_KEY"
      "AIRFLOW_APISERVER_BASE_URL"
      "AIRFLOW_APISERVER_HOST"
      "AIRFLOW_APISERVER_PORT_NUMBER"
      "AIRFLOW_LOAD_EXAMPLES"
      "AIRFLOW_HOSTNAME_CALLABLE"
      "AIRFLOW_POOL_NAME"
      "AIRFLOW_POOL_SIZE"
      "AIRFLOW_POOL_DESC"
      "AIRFLOW_STANDALONE_DAG_PROCESSOR"
      "AIRFLOW_TRIGGERER_DEFAULT_CAPACITY"
      "AIRFLOW_WORKER_QUEUE"
      "AIRFLOW_SKIP_DB_SETUP"
      "AIRFLOW_DB_MIGRATE_TIMEOUT"
      "AIRFLOW_ENABLE_HTTPS"
      "AIRFLOW_EXTERNAL_APISERVER_PORT_NUMBER"
      "AIRFLOW_DATABASE_HOST"
      "AIRFLOW_DATABASE_PORT_NUMBER"
      "AIRFLOW_DATABASE_NAME"
      "AIRFLOW_DATABASE_USERNAME"
      "AIRFLOW_DATABASE_PASSWORD"
      "AIRFLOW_DATABASE_USE_SSL"
      "AIRFLOW_REDIS_USE_SSL"
      "REDIS_HOST"
      "REDIS_PORT_NUMBER"
      "REDIS_USER"
      "REDIS_PASSWORD"
      "REDIS_DATABASE"
      "AIRFLOW_LDAP_ENABLE"
      "AIRFLOW_LDAP_URI"
      "AIRFLOW_LDAP_SEARCH"
      "AIRFLOW_LDAP_UID_FIELD"
      "AIRFLOW_LDAP_BIND_USER"
      "AIRFLOW_LDAP_BIND_PASSWORD"
      "AIRFLOW_LDAP_USER_REGISTRATION"
      "AIRFLOW_LDAP_USER_REGISTRATION_ROLE"
      "AIRFLOW_LDAP_ROLES_MAPPING"
      "AIRFLOW_LDAP_ROLES_SYNC_AT_LOGIN"
      "AIRFLOW_LDAP_USE_TLS"
      "AIRFLOW_LDAP_ALLOW_SELF_SIGNED"
      "AIRFLOW_LDAP_TLS_CA_CERTIFICATE"
    ];

    exposedPorts = lib.mkDefault [ 8080 8125 8793 8794 ];

    # Phase 4: enable in-image firestream-healthd. The airflow api-server
    # exposes `/api/v2/monitor/health` in 3.x (was `/health` in 2.x).
    # `AIRFLOW_APISERVER_PORT_NUMBER` defaults to 8080. Note: this health
    # check matters most for the `airflow` (api-server) image; other
    # components (scheduler/triggerer/etc.) share the image and so the
    # readiness command's relevance varies — they will still emit /readyz
    # responses but the path is only meaningful for the api-server role.
    health = {
      enable = lib.mkDefault true;
      readinessCmd = lib.mkDefault
        ''curl -fsS "http://localhost:''${AIRFLOW_APISERVER_PORT_NUMBER:-8080}/api/v2/monitor/health" > /dev/null'';
    };

    # image: schema defaults (repository = "firestream-airflow", tag = null -> version)
    # are already correct; do not set here.

    # Deployable docker-compose topology (rendered by nix/flake-modules/compose.nix
    # into packages.airflow-compose; driven by apps.airflow-up / apps.airflow-down).
    # This mirrors src/containers/firestream/airflow/docker-compose.yml, but the
    # postgresql/redis image tags are resolved from the flake registry ("@key") so
    # they always match what `.#postgresql` / `.#redis` build. Whole-block
    # mkDefault: a consumer override supplies its own complete topology.
    compose = lib.mkDefault {
      projectName = "firestream-airflow";
      dependencies = [ "postgresql" "redis" ];

      # x-airflow-env: shared across every Airflow component (own image services).
      # All component services must agree on these secret keys.
      sharedEnv = {
        AIRFLOW_DATABASE_NAME = "airflow";
        AIRFLOW_DATABASE_USERNAME = "airflow";
        AIRFLOW_DATABASE_PASSWORD = "airflow";
        AIRFLOW_EXECUTOR = "CeleryExecutor";
        AIRFLOW_APISERVER_HOST = "airflow";
        AIRFLOW_STANDALONE_DAG_PROCESSOR = "yes";
        AIRFLOW_USERNAME = "admin";
        AIRFLOW_PASSWORD = "admin";
        AIRFLOW_FERNET_KEY = "ZmlyZXN0cmVhbS1kZXYtZmVybmV0LWtleQ==";
        AIRFLOW_WEBSERVER_SECRET_KEY = "firestream-dev-webserver";
        AIRFLOW_APISERVER_SECRET_KEY = "firestream-dev-apiserver";
        AIRFLOW_JWT_SECRET_KEY = "firestream-dev-jwt-secret";
      };

      volumes = {
        postgresql_data = { };
        redis_data = { };
      };

      services = {
        # Dependency services: image resolved from the registry so the tag tracks
        # the built image. These do NOT receive sharedEnv (non-own images).
        postgresql = {
          image = "@postgresql";
          env = {
            POSTGRESQL_DATABASE = "airflow";
            POSTGRESQL_USERNAME = "airflow";
            POSTGRESQL_PASSWORD = "airflow";
            ALLOW_EMPTY_PASSWORD = "no";
          };
          # Publish on the host so the e2e harness (and humans) can probe the
          # embedded postgres without docker exec.
          ports = [ 5432 ];
          volumes = [ "postgresql_data:/firestream/postgresql" ];
          # Phase 4: query the in-image firestream-healthd /readyz endpoint
          # (port 9180 inside the compose network). The bash /dev/tcp pattern
          # is universal: every firestream image bundles bashInteractive, so
          # we don't need wget/curl/redis-cli/etc. in the path — just bash.
          # `127.0.0.1` is the container's own loopback, not the host's.
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
          # Published for the same reason as postgresql above.
          ports = [ 6379 ];
          volumes = [ "redis_data:/firestream/redis/data" ];
          # Phase 4: same /readyz bash /dev/tcp probe as postgresql. Redis
          # has no curl/wget but bashInteractive ships with every image.
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

        # Airflow components: image = null ⇒ own firestream-airflow image; each
        # gets sharedEnv merged with its component-specific overrides.
        airflow-scheduler = {
          env.AIRFLOW_COMPONENT_TYPE = "scheduler";
          dependsOn = [ "postgresql" "redis" ];
        };
        airflow-triggerer = {
          env.AIRFLOW_COMPONENT_TYPE = "triggerer";
          dependsOn = [ "postgresql" "redis" ];
        };
        airflow-dag-processor = {
          env.AIRFLOW_COMPONENT_TYPE = "dag-processor";
          dependsOn = [ "postgresql" "redis" ];
        };
        airflow-worker = {
          env.AIRFLOW_COMPONENT_TYPE = "worker";
          dependsOn = [ "postgresql" "redis" ];
        };
        airflow = {
          # api-server: bind to all interfaces and publish on the host.
          env.AIRFLOW_APISERVER_HOST = "0.0.0.0";
          # Phase 4: publish the api-server's in-image firestream-healthd
          # port too; the e2e harness's HealthEndpoint probe targets it.
          # (Default-branch stacks get this automatically via
          # `effectiveExposedPorts`; airflow declares its own services so
          # it must enumerate 9180 here too.)
          ports = [ 8080 9180 ];
          dependsOn = [ "postgresql" "redis" ];
          # Phase 4: explicit /readyz healthcheck on the api-server. The
          # rest of the airflow components share the same image and so each
          # serves /readyz too, but they're not service_healthy-gated by
          # this compose; only the dependents of postgresql/redis are.
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
      };
    };
  };
}
