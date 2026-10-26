# Supabase Storage Container Options
# Copyright Firestream. MIT License.
#
# The Supabase object storage API (`/storage/v1`), with SeaweedFS as its S3
# backend. Consumed by bin/nix/firestream/containers/eval-container.nix
# (runtimeType "system").
#
# Defaults are the upstream self-hosted v0.8.3 compose values with the S3
# backend settings from the SeaweedFS spike (FIRE-013). The S3 endpoint,
# keys and region default to the SeaweedFS chart's S3_LOCAL_* values. The
# bucket is dedicated to Storage and must exist before Storage starts.
#
# Secrets (DATABASE_URL holds the supabase_storage_admin password, ANON_KEY,
# SERVICE_KEY, AUTH_JWT_SECRET, AWS_SECRET_ACCESS_KEY, the S3 protocol keys)
# default to empty, except the published SeaweedFS dev key: compose and the
# chart supply them, and they accept the `_FILE` suffix.
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.storage = {
    version = lib.mkDefault "1.81.0";

    image.repository = lib.mkDefault "firestream-supabase-storage";

    paths = {
      base = lib.mkDefault "/opt/firestream/storage";
      conf = lib.mkDefault "/opt/firestream/storage/config";
      data = lib.mkDefault "/firestream/storage/data";
      logs = lib.mkDefault "/opt/firestream/storage/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      SERVER_HOST = "0.0.0.0";
      SERVER_PORT = "5000";

      DATABASE_URL = "";
      ANON_KEY = "";
      SERVICE_KEY = "";
      AUTH_JWT_SECRET = "";

      STORAGE_PUBLIC_URL = "http://localhost:8000";
      REQUEST_ALLOW_X_FORWARDED_PATH = "true";
      FILE_SIZE_LIMIT = "52428800";
      TENANT_ID = "stub";
      REGION = "us-east-1";

      STORAGE_BACKEND = "s3";
      GLOBAL_S3_BUCKET = "supabase-storage";
      # NOTE: the scheme belongs here; v1.81.0 does not read GLOBAL_S3_PROTOCOL.
      GLOBAL_S3_ENDPOINT = "http://seaweedfs-all-in-one.seaweedfs.svc.cluster.local:8333";
      # SeaweedFS serves buckets by path only.
      GLOBAL_S3_FORCE_PATH_STYLE = "true";
      STORAGE_S3_REGION = "us-east-1";
      AWS_ACCESS_KEY_ID = "firestream";
      AWS_SECRET_ACCESS_KEY = "firestream-secret";
      # WARNING: on means a second TUS upload with x-upsert to an existing key
      # fails with 409 (a Storage defect, seen on SeaweedFS and MinIO).
      STORAGE_VERSIONING_ENABLED = "false";

      ENABLE_IMAGE_TRANSFORMATION = "true";
      IMGPROXY_URL = "http://imgproxy:5001";

      S3_PROTOCOL_ACCESS_KEY_ID = "";
      S3_PROTOCOL_ACCESS_KEY_SECRET = "";
    };

    envSecrets = lib.mkDefault [
      "DATABASE_URL"
      "ANON_KEY"
      "SERVICE_KEY"
      "AUTH_JWT_SECRET"
      "AWS_SECRET_ACCESS_KEY"
      "S3_PROTOCOL_ACCESS_KEY_ID"
      "S3_PROTOCOL_ACCESS_KEY_SECRET"
    ];

    exposedPorts = lib.mkDefault [ 5000 ];

    health = {
      enable = lib.mkDefault true;
      # healthd runs outside the entrypoint env, so the port has a fallback.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/''${SERVER_PORT:-5000} && printf "GET /status HTTP/1.0\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 200"' '';
    };
  };
}
