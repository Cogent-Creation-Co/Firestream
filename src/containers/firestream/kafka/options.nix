# Kafka Container Options
# Copyright Firestream. MIT License.
#
# Externalized, declarative configuration for the Kafka container, consumed by
# bin/nix/firestream/containers/eval-container.nix. Defaults here are lifted
# VERBATIM from module.nix so that evalContainer's default build is byte-for-byte
# identical to the legacy flake.nix build path.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ lib, ... }:

{
  config.kafka = {
    version = lib.mkDefault "4.0";

    # Paths configuration (Bitnami compatibility)
    # Per-key mkDefault so individual paths can be overridden independently.
    paths = {
      base = lib.mkDefault "/opt/firestream/kafka";
      conf = lib.mkDefault "/opt/firestream/kafka/config";
      data = lib.mkDefault "/firestream/kafka/data";
      logs = lib.mkDefault "/opt/firestream/kafka/logs";
    };

    # Environment variables with defaults
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    env = builtins.mapAttrs (_: lib.mkDefault) {
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
      # callers override these via the standard mkDefault seam.
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
    };

    # Variables supporting _FILE suffix for Docker secrets.
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
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
    ];

    exposedPorts = lib.mkDefault [ 9092 9093 ];

    # Phase 4: enable in-image firestream-healthd. The readinessCmd asks the
    # broker for its API versions over the configured client listener — this
    # proves metadata is serviceable, not just that the listener has bound.
    # `KAFKA_PORT_NUMBER` is the Bitnami client-listener env var (see env
    # defaults above; defaults to 9092). The `.sh` suffix is the actual
    # binary name in pkgs.apacheKafka/bin.
    health = {
      enable = lib.mkDefault true;
      readinessCmd = lib.mkDefault
        ''kafka-broker-api-versions.sh --bootstrap-server "localhost:''${KAFKA_PORT_NUMBER:-9092}"'';
    };
  };
}
