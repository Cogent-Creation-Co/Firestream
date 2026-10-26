# Supabase Auth (GoTrue) Container Options
# Copyright Firestream. MIT License.
#
# The Supabase auth API (`/auth/v1`). Consumed by
# bin/nix/firestream/containers/eval-container.nix (runtimeType "system").
#
# Defaults are the upstream self-hosted v0.8.3 compose values. Secrets
# (GOTRUE_DB_DATABASE_URL holds the supabase_auth_admin password,
# GOTRUE_JWT_SECRET, GOTRUE_SMTP_PASS) default to empty: compose and the chart
# supply them, and they accept the `_FILE` suffix.
#
# WARNING: never default a numeric or boolean GOTRUE_* variable to "". The
# entrypoint exports every key here, and GoTrue fails at boot when it parses
# an empty string as a number or a boolean. Leave such a key out instead
# (for example GOTRUE_JWT_KEYS, which a consumer adds when it uses the
# asymmetric keys).
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

{
  config.gotrue = {
    version = lib.mkDefault "2.197.0";

    image.repository = lib.mkDefault "firestream-supabase-auth";

    paths = {
      base = lib.mkDefault "/opt/firestream/gotrue";
      conf = lib.mkDefault "/opt/firestream/gotrue/config";
      data = lib.mkDefault "/firestream/gotrue/data";
      logs = lib.mkDefault "/opt/firestream/gotrue/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) {
      GOTRUE_API_HOST = "0.0.0.0";
      GOTRUE_API_PORT = "9999";
      # The public URL of this API, including the gateway prefix.
      API_EXTERNAL_URL = "http://localhost:8000/auth/v1";

      GOTRUE_DB_DRIVER = "postgres";
      GOTRUE_DB_DATABASE_URL = "";

      GOTRUE_SITE_URL = "http://localhost:3000";
      GOTRUE_URI_ALLOW_LIST = "";
      GOTRUE_DISABLE_SIGNUP = "false";

      GOTRUE_JWT_ADMIN_ROLES = "service_role";
      GOTRUE_JWT_AUD = "authenticated";
      GOTRUE_JWT_DEFAULT_GROUP_NAME = "authenticated";
      GOTRUE_JWT_EXP = "3600";
      GOTRUE_JWT_SECRET = "";
      # Empty means API_EXTERNAL_URL (the upstream value), set at boot.
      GOTRUE_JWT_ISSUER = "";

      GOTRUE_EXTERNAL_EMAIL_ENABLED = "true";
      GOTRUE_EXTERNAL_ANONYMOUS_USERS_ENABLED = "false";
      GOTRUE_MAILER_AUTOCONFIRM = "false";
      GOTRUE_EXTERNAL_PHONE_ENABLED = "true";
      GOTRUE_SMS_AUTOCONFIRM = "true";

      GOTRUE_SMTP_ADMIN_EMAIL = "admin@example.com";
      GOTRUE_SMTP_HOST = "";
      GOTRUE_SMTP_PORT = "587";
      GOTRUE_SMTP_USER = "";
      GOTRUE_SMTP_PASS = "";
      GOTRUE_SMTP_SENDER_NAME = "";
      GOTRUE_MAILER_URLPATHS_INVITE = "/auth/v1/verify";
      GOTRUE_MAILER_URLPATHS_CONFIRMATION = "/auth/v1/verify";
      GOTRUE_MAILER_URLPATHS_RECOVERY = "/auth/v1/verify";
      GOTRUE_MAILER_URLPATHS_EMAIL_CHANGE = "/auth/v1/verify";
    };

    envSecrets = lib.mkDefault [
      "GOTRUE_DB_DATABASE_URL"
      "GOTRUE_JWT_SECRET"
      "GOTRUE_SMTP_PASS"
    ];

    exposedPorts = lib.mkDefault [ 9999 ];

    health = {
      enable = lib.mkDefault true;
      # healthd runs outside the entrypoint env, so the port has a fallback.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/''${GOTRUE_API_PORT:-9999} && printf "GET /health HTTP/1.0\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 200"' '';
    };
  };
}
