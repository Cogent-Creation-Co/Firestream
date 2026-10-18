# Odoo chart options: `backup.*` (database + filestore dump CronJob).
#
# `backup` is NOT a Bitnami value: the vendored odoo chart has no backup
# template. The chart flake-module (nix/flake-modules/charts/odoo.nix) appends
# a templated CronJob to `extraDeploy` that is guarded by
# `{{ if .Values.backup.enabled }}`, runs the firestream-odoo image with the
# data PVC mounted, and calls `odoo_backup_s3` from libhelpersodoo.sh. The
# block therefore lands in values.yaml and every field here is a helm-time
# value (`--set backup.enabled=true` works).
#
# The `s3` block is the same shape as the postgresql chart's `backup.s3`
# (src/charts/firestream/postgresql/nix/options/backup.nix) so one consumer
# override style covers both charts. Defaults target the in-cluster SeaweedFS
# dev store; set `existingSecret`, clear `endpoint`, and switch
# `addressingStyle` for real cloud S3.
{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.odoo.backup = mkOption {
    default = {};
    description = "Odoo database + filestore backup CronJob configuration";
    type = types.submodule {
      options = {
        enabled = mkOption {
          type = types.bool;
          default = false;
          description = "Render the backup CronJob into extraDeploy.";
        };

        schedule = mkOption {
          type = types.str;
          default = "@daily";
          description = "Cron schedule for the backup CronJob.";
        };

        ttlSecondsAfterFinished = mkOption {
          type = types.nullOr types.int;
          default = 86400;
          description = "Delete finished backup Jobs after this many seconds. Null keeps them.";
        };

        successfulJobsHistoryLimit = mkOption {
          type = types.int;
          default = 3;
          description = "Completed Jobs retained by the CronJob.";
        };

        failedJobsHistoryLimit = mkOption {
          type = types.int;
          default = 3;
          description = "Failed Jobs retained by the CronJob.";
        };

        resources = mkOption {
          type = types.attrsOf types.anything;
          default = {};
          description = "Pod resources for the backup container (requests/limits).";
        };

        s3 = mkOption {
          description = ''
            S3 backup target. Consumed by the chart flake-module to build the
            backup CronJob env. Defaults to the in-cluster SeaweedFS store. To
            target real cloud S3: set `existingSecret`, clear `endpoint`
            (null), and set `addressingStyle = "auto"`.
          '';
          default = {};
          type = types.submodule {
            options = {
              endpoint = mkOption {
                type = types.nullOr types.str;
                default = "http://seaweedfs-all-in-one.seaweedfs.svc.cluster.local:8333";
                description = ''
                  S3 endpoint URL. Passed to `aws --endpoint-url` only when
                  non-null/non-empty. Set to null for AWS S3.
                '';
              };
              bucket = mkOption {
                type = types.str;
                default = "firestream";
                description = "Destination bucket for archives.";
              };
              prefix = mkOption {
                type = types.str;
                default = "odoo-backups";
                description = "Key prefix within the bucket.";
              };
              region = mkOption {
                type = types.str;
                default = "us-east-1";
                description = "AWS_DEFAULT_REGION for the aws CLI.";
              };
              addressingStyle = mkOption {
                type = types.nullOr (types.enum [ "path" "virtual" "auto" ]);
                default = "path";
                description = ''
                  S3 addressing style. "path" for SeaweedFS/MinIO; "auto" or
                  "virtual" for AWS S3.
                '';
              };
              existingSecret = mkOption {
                type = types.nullOr types.str;
                default = null;
                description = ''
                  Name of a Secret IN THE RELEASE NAMESPACE holding the S3
                  credentials. When set, creds are injected via secretKeyRef
                  instead of the literal accessKeyId/secretAccessKey below.
                '';
              };
              accessKeyIdKey = mkOption {
                type = types.str;
                default = "AWS_ACCESS_KEY_ID";
                description = "Key within existingSecret holding the access key id.";
              };
              secretAccessKeyKey = mkOption {
                type = types.str;
                default = "AWS_SECRET_ACCESS_KEY";
                description = "Key within existingSecret holding the secret access key.";
              };
              accessKeyId = mkOption {
                type = types.nullOr types.str;
                default = "firestream";
                description = "Literal access key id (dev only). Ignored when existingSecret is set.";
              };
              secretAccessKey = mkOption {
                type = types.nullOr types.str;
                default = "firestream-secret";
                description = "Literal secret access key (dev only). Ignored when existingSecret is set.";
              };
            };
          };
        };
      };
    };
  };
}
