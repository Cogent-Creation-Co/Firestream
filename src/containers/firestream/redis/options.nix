# Redis Container Options (shared base)
# Copyright Firestream. MIT License.
#
# Externalized, declarative SHARED configuration for the Redis container,
# consumed by bin/nix/firestream/containers/eval-container.nix. Defaults here are
# lifted VERBATIM from module.nix so that evalContainer's default build is
# byte-for-byte identical to the legacy flake.nix build path.
#
# NOTE: Redis is MULTI-VERSION (7/8). `version` varies per build and is supplied
# by the flake-module via an inline override module, NOT here. The Phase B
# default alias (`redis`) now points at v8 — matching the Bitnami redis chart's
# expected protocol — while `redis-7` remains available for consumer downgrade.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ lib, ... }:

{
  config.redis = {
    # Paths configuration (Bitnami compatibility)
    # Per-key mkDefault so individual paths can be overridden independently.
    paths = {
      base = lib.mkDefault "/opt/firestream/redis";
      conf = lib.mkDefault "/opt/firestream/redis/etc";
      data = lib.mkDefault "/firestream/redis/data";
      logs = lib.mkDefault "/opt/firestream/redis/logs";
    };

    # Environment variables with defaults
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    env = builtins.mapAttrs (_: lib.mkDefault) {
      # Base directories
      REDIS_BASE_DIR = "/opt/firestream/redis";
      REDIS_VOLUME_DIR = "/firestream/redis";
      REDIS_DATA_DIR = "/firestream/redis/data";
      REDIS_CONF_DIR = "/opt/firestream/redis/etc";
      REDIS_DEFAULT_CONF_DIR = "/opt/firestream/redis/etc.default";
      REDIS_MOUNTED_CONF_DIR = "/opt/firestream/redis/mounted-etc";
      REDIS_OVERRIDES_FILE = "/opt/firestream/redis/mounted-etc/overrides.conf";
      REDIS_CONF_FILE = "/opt/firestream/redis/etc/redis.conf";
      REDIS_LOG_DIR = "/opt/firestream/redis/logs";
      REDIS_LOG_FILE = "/opt/firestream/redis/logs/redis.log";
      REDIS_TMP_DIR = "/opt/firestream/redis/tmp";
      REDIS_PID_FILE = "/opt/firestream/redis/tmp/redis.pid";
      REDIS_BIN_DIR = "/opt/firestream/redis/bin";

      # User and group
      REDIS_DAEMON_USER = "redis";
      REDIS_DAEMON_GROUP = "redis";

      # Connection settings
      REDIS_PORT_NUMBER = "6379";
      REDIS_ALLOW_REMOTE_CONNECTIONS = "yes";
      REDIS_EXTRA_FLAGS = "";

      # Authentication
      # Default ALLOW_EMPTY_PASSWORD=yes for out-of-the-box local/dev/e2e use;
      # production overrides via the standard mkDefault seam.
      REDIS_PASSWORD = "";
      ALLOW_EMPTY_PASSWORD = "yes";

      # Persistence
      REDIS_AOF_ENABLED = "yes";
      REDIS_RDB_POLICY = "";
      REDIS_RDB_POLICY_DISABLED = "no";

      # Replication
      REDIS_REPLICATION_MODE = "";
      REDIS_MASTER_HOST = "";
      REDIS_MASTER_PORT_NUMBER = "6379";
      REDIS_MASTER_PASSWORD = "";
      REDIS_REPLICA_IP = "";
      REDIS_REPLICA_PORT = "";

      # ACL
      REDIS_ACLFILE = "";
      REDIS_DISABLE_COMMANDS = "";

      # Multi-threading
      REDIS_IO_THREADS = "";
      REDIS_IO_THREADS_DO_READS = "";

      # TLS
      REDIS_TLS_ENABLED = "no";
      REDIS_TLS_PORT_NUMBER = "6379";
      REDIS_TLS_CERT_FILE = "";
      REDIS_TLS_KEY_FILE = "";
      REDIS_TLS_KEY_FILE_PASS = "";
      REDIS_TLS_CA_FILE = "";
      REDIS_TLS_CA_DIR = "";
      REDIS_TLS_DH_PARAMS_FILE = "";
      REDIS_TLS_AUTH_CLIENTS = "yes";

      # Sentinel
      REDIS_SENTINEL_HOST = "";
      REDIS_SENTINEL_PORT_NUMBER = "26379";
      REDIS_SENTINEL_MASTER_NAME = "";

      # Debug mode
      BITNAMI_DEBUG = "false";
    };

    # Variables supporting _FILE suffix for Docker secrets.
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
      "REDIS_PASSWORD"
      "REDIS_MASTER_PASSWORD"
      "REDIS_TLS_KEY_FILE_PASS"
    ];

    exposedPorts = lib.mkDefault [ 6379 ];

    # Phase 4: enable in-image firestream-healthd. The readinessCmd is passed
    # to healthd as a single string and evaluated via `sh -c` at probe time,
    # so `${REDIS_PORT_NUMBER:-6379}` expands at runtime against the
    # container's env (Bitnami's standard env var; see env defaults above).
    health = {
      enable = lib.mkDefault true;
      readinessCmd = lib.mkDefault
        ''redis-cli -p "''${REDIS_PORT_NUMBER:-6379}" ping | grep -q PONG'';
    };
  };
}
