# imgproxy Container Options
# Copyright Firestream. MIT License.
#
# The image transformer behind Supabase Storage (`ENABLE_IMAGE_TRANSFORMATION`).
# Consumed by bin/nix/firestream/containers/eval-container.nix
# (runtimeType "system").
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.imgproxy = {
    version = lib.mkDefault "3.31.4";

    image.repository = lib.mkDefault "firestream-supabase-imgproxy";

    paths = {
      base = lib.mkDefault "/opt/firestream/imgproxy";
      conf = lib.mkDefault "/opt/firestream/imgproxy/config";
      data = lib.mkDefault "/firestream/imgproxy/data";
      logs = lib.mkDefault "/opt/firestream/imgproxy/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      IMGPROXY_BIND = ":5001";
      # NOTE: with the file backend, Storage asks for `local:///<path>`, so
      # imgproxy mounts the Storage volume at the same path (upstream compose).
      IMGPROXY_LOCAL_FILESYSTEM_ROOT = "/";
      IMGPROXY_USE_ETAG = "true";
      IMGPROXY_AUTO_WEBP = "true";
      IMGPROXY_MAX_SRC_RESOLUTION = "16.8";

      # Direct `s3://` sources. Storage does not need them; they are off.
      IMGPROXY_USE_S3 = "false";
      IMGPROXY_S3_ENDPOINT = "";
      IMGPROXY_S3_REGION = "";
      AWS_ACCESS_KEY_ID = "";
      AWS_SECRET_ACCESS_KEY = "";
    };

    envSecrets = lib.mkDefault [
      "AWS_ACCESS_KEY_ID"
      "AWS_SECRET_ACCESS_KEY"
    ];

    exposedPorts = lib.mkDefault [ 5001 ];

    health = {
      enable = lib.mkDefault true;
      # NOTE: `imgproxy health` reads IMGPROXY_BIND, which healthd does not
      # inherit from the entrypoint, so the probe names the default.
      readinessCmd = lib.mkDefault ''IMGPROXY_BIND="''${IMGPROXY_BIND:-:5001}" imgproxy health'';
    };
  };
}
