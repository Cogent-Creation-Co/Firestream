# Spark Container Options
# Copyright Firestream. MIT License.
#
# Externalized, declarative configuration for the Apache Spark container,
# consumed by bin/nix/firestream/containers/eval-container.nix. Defaults here are
# lifted VERBATIM from module.nix so that evalContainer's default build is
# byte-for-byte identical to the legacy flake.nix build path.
#
# NOTE: `jdk` and `python` remain module defaults inside module.nix (NOT typed
# options here); the flake-module does not pass them. The python-derived env
# values below reference `pkgs.python312` to match the module's `python` default.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ lib, pkgs, ... }:

let
  # Matches module.nix's `python ? pkgs.python312` default.
  python = pkgs.python312;
in
{
  config.spark = {
    version = lib.mkDefault "4.0.0";

    # Paths configuration
    # Per-key mkDefault so individual paths can be overridden independently.
    paths = {
      base = lib.mkDefault "/opt/firestream/spark";
      conf = lib.mkDefault "/opt/firestream/spark/conf";
      data = lib.mkDefault "/firestream/spark/data";
      logs = lib.mkDefault "/opt/firestream/spark/logs";
    };

    # Environment variables with defaults
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    env = builtins.mapAttrs (_: lib.mkDefault) {
      # Paths (Bitnami compatibility)
      BITNAMI_ROOT_DIR = "/opt/bitnami";
      BITNAMI_VOLUME_DIR = "/bitnami";

      # Firestream paths
      FIRESTREAM_ROOT_DIR = "/opt/firestream";
      FIRESTREAM_VOLUME_DIR = "/firestream";

      # Spark paths
      SPARK_HOME = "/opt/firestream/spark";
      SPARK_BASE_DIR = "/opt/firestream/spark";
      SPARK_CONF_DIR = "/opt/firestream/spark/conf";
      SPARK_DEFAULT_CONF_DIR = "/opt/firestream/spark/conf.default";
      SPARK_CONF_FILE = "/opt/firestream/spark/conf/spark-defaults.conf";
      SPARK_WORK_DIR = "/opt/firestream/spark/work";
      SPARK_LOG_DIR = "/opt/firestream/spark/logs";
      SPARK_TMP_DIR = "/opt/firestream/spark/tmp";
      SPARK_JARS_DIR = "/opt/firestream/spark/jars";
      SPARK_USER_JARS_DIR = "/firestream/spark/jars";
      SPARK_DATA_DIR = "/firestream/spark/data";
      SPARK_INITSCRIPTS_DIR = "/docker-entrypoint-initdb.d";

      # Spark mode
      SPARK_MODE = "master";
      SPARK_MASTER_URL = "spark://spark-master:7077";
      SPARK_NO_DAEMONIZE = "true";

      # User configuration
      SPARK_USER = "spark";
      SPARK_DAEMON_USER = "spark";
      SPARK_DAEMON_GROUP = "spark";

      # Security defaults (disabled)
      SPARK_RPC_AUTHENTICATION_ENABLED = "no";
      SPARK_RPC_ENCRYPTION_ENABLED = "no";
      SPARK_LOCAL_STORAGE_ENCRYPTION_ENABLED = "no";
      SPARK_SSL_ENABLED = "no";
      SPARK_SSL_NEED_CLIENT_AUTH = "yes";
      SPARK_SSL_PROTOCOL = "TLSv1.2";
      SPARK_METRICS_ENABLED = "false";

      # Note: JAVA_HOME is auto-set by mkJavaContainerModule

      # Python configuration
      PYTHONPATH = "/opt/firestream/spark/python:/opt/firestream/spark/python/lib/py4j-0.10.9.7-src.zip";
      PYSPARK_PYTHON = "${python}/bin/python3";
      PYSPARK_DRIVER_PYTHON = "${python}/bin/python3";

      # Debug mode
      BITNAMI_DEBUG = "false";
      MODULE = "spark";
    };

    # Variables that support Docker secrets (_FILE suffix).
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
      "SPARK_MODE"
      "SPARK_MASTER_URL"
      "SPARK_RPC_AUTHENTICATION_ENABLED"
      "SPARK_RPC_AUTHENTICATION_SECRET"
      "SPARK_RPC_ENCRYPTION_ENABLED"
      "SPARK_LOCAL_STORAGE_ENCRYPTION_ENABLED"
      "SPARK_SSL_ENABLED"
      "SPARK_SSL_KEY_PASSWORD"
      "SPARK_SSL_KEYSTORE_PASSWORD"
      "SPARK_SSL_KEYSTORE_FILE"
      "SPARK_SSL_TRUSTSTORE_PASSWORD"
      "SPARK_SSL_TRUSTSTORE_FILE"
      "SPARK_SSL_NEED_CLIENT_AUTH"
      "SPARK_SSL_PROTOCOL"
      "SPARK_WEBUI_SSL_PORT"
      "SPARK_METRICS_ENABLED"
    ];

    exposedPorts = lib.mkDefault [
      7077   # Spark master
      8080   # Spark master UI
      8081   # Spark worker UI
      4040   # Spark application UI
      6066   # Spark REST submission port
    ];

    # Phase 4: enable in-image firestream-healthd. The master and worker UIs
    # default to 8080 (master) / 8081 (worker). Bitnami's spark image
    # honours `SPARK_MASTER_WEBUI_PORT` / `SPARK_WORKER_WEBUI_PORT` if set,
    # both falling back to 8080 by default in master mode; the curl probe
    # `?` short-circuits a non-200 with `--fail`. spark has `curl` in
    # systemDeps so this is in PATH.
    health = {
      enable = lib.mkDefault true;
      readinessCmd = lib.mkDefault
        ''curl -fsS "http://localhost:''${SPARK_MASTER_WEBUI_PORT:-8080}/" > /dev/null'';
    };
  };
}
