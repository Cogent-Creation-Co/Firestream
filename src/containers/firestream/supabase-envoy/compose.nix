# Supabase docker-compose stack
# Copyright Firestream. MIT License.
#
# The complete self-hosted Supabase stack, declared on the gateway image (the
# stack's single public entry point) and rendered by
# nix/flake-modules/compose.nix into packages.supabase-envoy-compose. The
# Supabase flake-module (nix/flake-modules/supabase.nix) publishes it as
# packages.supabase-compose and apps.supabase-up / apps.supabase-down.
#
# Takes `devSecrets` (firestreamLib.supabase.devSecrets): a fixed, published
# key set for local development only. Never reuse it on a reachable host;
# production keys come from `nix run .#supabase-keygen`.
#
# Start order (each arrow is a compose `depends_on` gate):
#   seaweedfs -> seaweedfs-buckets (one-shot, must exit 0)
#   db -> rest, auth, realtime, supavisor, meta; imgproxy has no gate
#   rest, meta, imgproxy, seaweedfs-buckets -> storage
#   meta -> studio
#   rest, auth, realtime, supavisor, storage, studio -> api-gw
#
# Published container ports (host ports: stack "supabase" in
# bin/nix/firestream/ports.nix):
#   api-gw    8000   Studio, /rest/v1, /auth/v1, /storage/v1, /realtime/v1
#   api-gw    9180   firestream-healthd of the gateway
#   db        5432   direct Postgres
#   supavisor 6543   transaction-mode pooler (user `<role>.firestream`)

{ devSecrets }:
{ lib, ... }:

let
  s = devSecrets;

  ports = (import ../../../../bin/nix/firestream/ports-lib.nix { inherit lib; }).registry;
  gatewayHostPort = ports.hostPort { stack = "supabase"; service = "api-gw"; container = 8000; };
  publicUrl = "http://localhost:${toString gatewayHostPort}";

  dbUrl = user: "postgres://${user}:${s.POSTGRES_PASSWORD}@db:5432/postgres";

  # NOTE: healthd /readyz runs each image's own readiness command
  # (pg_isready, PostgREST admin /ready, ...). The test uses bash built-ins
  # only, because the imgproxy image has no grep or head. `$$` is compose's
  # escape for a literal `$`.
  readyz = {
    test = [
      "CMD"
      "bash"
      "-c"
      "exec 3<>/dev/tcp/127.0.0.1/9180 && printf 'GET /readyz HTTP/1.0\\r\\n\\r\\n' >&3 && read -r status <&3 && [[ $$status == *' 200 '* ]]"
    ];
    interval = "5s";
    timeout = "5s";
    retries = 30;
    # NOTE: the first boot of db runs every Supabase migration. Compose never
    # starts a dependent of a service that turned unhealthy, so a short grace
    # period fails `up` on a slow first boot.
    start_period = "180s";
  };

  sw = "-master=seaweedfs:9333 -filer=seaweedfs:8888";
  bucket = "supabase-storage";
  s3 = { accessKey = "firestream"; secretKey = "firestream-secret"; };
in
{
  config.envoy.compose = lib.mkDefault {
    projectName = "firestream-supabase";
    dependencies = [
      "supabase-postgres"
      "seaweedfs"
      "supabase-postgrest"
      "supabase-auth"
      "supabase-realtime"
      "supabase-supavisor"
      "supabase-postgres-meta"
      "supabase-imgproxy"
      "supabase-storage"
      "supabase-studio"
    ];

    volumes = {
      db_data = { };
      seaweedfs_data = { };
      studio_data = { };
    };

    services = {
      db = {
        image = "@supabase-postgres";
        env.POSTGRESQL_PASSWORD = s.POSTGRES_PASSWORD;
        ports = [ 5432 ];
        # NOTE: pgsodium_root.key is created in this directory on first boot.
        # Losing the volume loses the key, and every pgsodium/Vault secret
        # encrypted with it becomes unreadable.
        volumes = [ "db_data:/firestream/postgresql" ];
        healthcheck = readyz;
      };

      # WARNING: weed turns AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY into an
      # admin identity. Without them, S3 accepts anonymous requests.
      seaweedfs = {
        image = "@seaweedfs";
        env = {
          AWS_ACCESS_KEY_ID = s3.accessKey;
          AWS_SECRET_ACCESS_KEY = s3.secretKey;
        };
        volumes = [ "seaweedfs_data:/firestream/seaweedfs" ];
        healthcheck = readyz // {
          test = [ "CMD" "bash" "-c" "exec 3<>/dev/tcp/127.0.0.1/8333" ];
          start_period = "60s";
        };
      };

      # NOTE: Storage needs its bucket to exist and does not create it.
      # Compose interpolates `$` in this string, so the script holds none.
      seaweedfs-buckets = {
        image = "@seaweedfs";
        command = [
          "bash"
          "-c"
          (lib.concatStringsSep " " [
            "for attempt in {1..30}; do"
            "echo s3.bucket.list | weed shell ${sw} | grep -qw ${bucket} && exit 0;"
            "echo 's3.bucket.create -name ${bucket}' | weed shell ${sw} || true;"
            "sleep 2;"
            "done; exit 1"
          ])
        ];
        dependsOn = [ "seaweedfs" ];
      };

      rest = {
        image = "@supabase-postgrest";
        env = {
          PGRST_DB_URI = dbUrl "authenticator";
          PGRST_JWT_SECRET = s.JWT_JWKS;
        };
        dependsOn = [ "db" ];
        healthcheck = readyz;
      };

      auth = {
        image = "@supabase-auth";
        env = {
          GOTRUE_DB_DATABASE_URL = dbUrl "supabase_auth_admin";
          GOTRUE_JWT_SECRET = s.JWT_SECRET;
          GOTRUE_JWT_KEYS = s.JWT_KEYS;
          API_EXTERNAL_URL = "${publicUrl}/auth/v1";
          GOTRUE_JWT_ISSUER = "${publicUrl}/auth/v1";
          # NOTE: the dev stack has no SMTP server, so sign-ups confirm at once.
          GOTRUE_MAILER_AUTOCONFIRM = "true";
        };
        dependsOn = [ "db" ];
        healthcheck = readyz;
      };

      realtime = {
        image = "@supabase-realtime";
        env = {
          DB_PASSWORD = s.POSTGRES_PASSWORD;
          DB_ENC_KEY = s.REALTIME_DB_ENC_KEY;
          API_JWT_SECRET = s.JWT_SECRET;
          # NOTE: the seed copies this into the tenant on every boot. Without
          # it, Realtime cannot verify the ES256 token that the gateway puts
          # in place of an `sb_publishable_*` key.
          API_JWT_JWKS = s.JWT_JWKS;
          METRICS_JWT_SECRET = s.JWT_SECRET;
          SECRET_KEY_BASE = s.SECRET_KEY_BASE;
        };
        dependsOn = [ "db" ];
        healthcheck = readyz;
      };

      supavisor = {
        image = "@supabase-supavisor";
        env = {
          POSTGRES_PASSWORD = s.POSTGRES_PASSWORD;
          SECRET_KEY_BASE = s.SECRET_KEY_BASE;
          VAULT_ENC_KEY = s.VAULT_ENC_KEY;
          API_JWT_SECRET = s.JWT_SECRET;
          METRICS_JWT_SECRET = s.JWT_SECRET;
        };
        ports = [ 6543 ];
        dependsOn = [ "db" ];
        healthcheck = readyz;
      };

      meta = {
        image = "@supabase-postgres-meta";
        env = {
          PG_META_DB_PASSWORD = s.POSTGRES_PASSWORD;
          CRYPTO_KEY = s.PG_META_CRYPTO_KEY;
        };
        dependsOn = [ "db" ];
        healthcheck = readyz;
      };

      imgproxy = {
        image = "@supabase-imgproxy";
        healthcheck = readyz;
      };

      storage = {
        image = "@supabase-storage";
        env = {
          DATABASE_URL = dbUrl "supabase_storage_admin";
          ANON_KEY = s.ANON_KEY;
          SERVICE_KEY = s.SERVICE_ROLE_KEY;
          AUTH_JWT_SECRET = s.JWT_SECRET;
          JWT_JWKS = s.JWT_JWKS;
          POSTGREST_URL = "http://rest:3000";
          STORAGE_PUBLIC_URL = publicUrl;
          GLOBAL_S3_BUCKET = bucket;
          GLOBAL_S3_ENDPOINT = "http://seaweedfs:8333";
          AWS_ACCESS_KEY_ID = s3.accessKey;
          AWS_SECRET_ACCESS_KEY = s3.secretKey;
        };
        dependsOn = [ "rest" "meta" "imgproxy" ];
        dependsOnCompleted = [ "seaweedfs-buckets" ];
        healthcheck = readyz;
      };

      studio = {
        image = "@supabase-studio";
        env = {
          POSTGRES_PASSWORD = s.POSTGRES_PASSWORD;
          PG_META_CRYPTO_KEY = s.PG_META_CRYPTO_KEY;
          SUPABASE_PUBLIC_URL = publicUrl;
          SUPABASE_ANON_KEY = s.ANON_KEY;
          SUPABASE_SERVICE_KEY = s.SERVICE_ROLE_KEY;
          SUPABASE_PUBLISHABLE_KEY = s.SUPABASE_PUBLISHABLE_KEY;
          SUPABASE_SECRET_KEY = s.SUPABASE_SECRET_KEY;
          AUTH_JWT_SECRET = s.JWT_SECRET;
        };
        volumes = [ "studio_data:/firestream/studio/data" ];
        dependsOn = [ "meta" ];
        healthcheck = readyz;
      };

      # NOTE: the service name is the upstream api-gw, because that is the
      # SUPABASE_URL default of the Studio image.
      api-gw = {
        env = {
          ANON_KEY = s.ANON_KEY;
          SERVICE_ROLE_KEY = s.SERVICE_ROLE_KEY;
          SUPABASE_PUBLISHABLE_KEY = s.SUPABASE_PUBLISHABLE_KEY;
          SUPABASE_SECRET_KEY = s.SUPABASE_SECRET_KEY;
          ANON_KEY_ASYMMETRIC = s.ANON_KEY_ASYMMETRIC;
          SERVICE_ROLE_KEY_ASYMMETRIC = s.SERVICE_ROLE_KEY_ASYMMETRIC;
          SUPABASE_PUBLIC_URL = publicUrl;
          DASHBOARD_USERNAME = s.DASHBOARD_USERNAME;
          DASHBOARD_PASSWORD = s.DASHBOARD_PASSWORD;
          # NOTE: the upstream service is `realtime`; the Host header that
          # names the tenant stays ENVOY_REALTIME_TENANT_HOST.
          ENVOY_REALTIME_HOST = "realtime";
        };
        ports = [ 8000 9180 ];
        dependsOn = [ "rest" "auth" "realtime" "supavisor" "storage" "studio" ];
        healthcheck = readyz;
      };
    };
  };
}
