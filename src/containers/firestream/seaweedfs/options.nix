# SeaweedFS Container Options (shared base)
# Copyright Firestream. MIT License.
#
# Externalized, declarative configuration for the SeaweedFS container, consumed
# by bin/nix/firestream/containers/eval-container.nix. Defaults here mirror the
# literals in module.nix so evalContainer's default build matches the
# direct-import path.
#
# SeaweedFS is the Firestream object store (9th supported app): a single Go
# binary (`weed`) providing an S3-compatible API. It is NOT a Bitnami chart and
# never sources /opt/bitnami/scripts/lib*.sh, so there are no per-container
# helpers / config / init scripts — the container is simply "weed on PATH".
#
# SINGLE-VERSION: unlike redis/postgresql there is no N/M split, so `version` is
# pinned here directly.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault (each value wrapped
# individually). A single mkDefault around the whole attrset would be replaced
# wholesale when a consumer overrides one key, silently dropping siblings.

{ lib, ... }:

{
  config.seaweedfs = {
    # Pinned upstream describe (seaweedfs/seaweedfs @ 5797fb24; see
    # bin/nix/firestream/packages/seaweedfs-weed.nix).
    version = lib.mkDefault "4.36-17-g5797fb24e";

    # Paths configuration (Firestream FHS). weed does not consume these, but the
    # env-defaults generator (bin/nix/firestream/env/defaults.nix) requires the
    # base/conf/data/logs keys to be present.
    paths = {
      base = lib.mkDefault "/opt/firestream/seaweedfs";
      conf = lib.mkDefault "/opt/firestream/seaweedfs/config";
      data = lib.mkDefault "/firestream/seaweedfs/data";
      logs = lib.mkDefault "/opt/firestream/seaweedfs/logs";
    };

    # Environment variables with defaults.
    # CRITICAL: per-leaf mkDefault (wrap each value), NOT a whole-set mkDefault.
    # The S3 admin credentials are surfaced so they can be loaded from _FILE
    # secrets (see envSecrets below).
    env = builtins.mapAttrs (_: lib.mkDefault) {
      AWS_ACCESS_KEY_ID = "";
      AWS_SECRET_ACCESS_KEY = "";
    };

    # Variables supporting the _FILE suffix for Docker/K8s secrets.
    # Whole-value mkDefault is correct for lists (replacement semantics).
    envSecrets = lib.mkDefault [
      "AWS_ACCESS_KEY_ID"
      "AWS_SECRET_ACCESS_KEY"
    ];

    # s3 (8333), master (9333), filer (8888), volume (8080), metrics (9324).
    exposedPorts = lib.mkDefault [ 8333 9333 8888 8080 9324 ];
  };
}
