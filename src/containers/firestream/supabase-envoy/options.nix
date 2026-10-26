# Supabase API gateway (Envoy) Container Options
# Copyright Firestream. MIT License.
#
# The single public entry point of a Supabase stack (port 8000): it routes
# /auth/v1, /rest/v1, /graphql/v1, /realtime/v1, /storage/v1, /functions/v1,
# /pg/ and Studio, checks the `apikey`, and translates sb_publishable_* and
# sb_secret_* keys into the internal JWTs. Consumed by
# bin/nix/firestream/containers/eval-container.nix (runtimeType "system").
#
# The keys default to empty: compose and the chart supply them from
# `firestreamLib.supabase.devSecrets` or a generated set, and they accept the
# `_FILE` suffix. The sb_ key translation is on only when all four of
# SUPABASE_PUBLISHABLE_KEY, SUPABASE_SECRET_KEY, ANON_KEY_ASYMMETRIC and
# SERVICE_ROLE_KEY_ASYMMETRIC are set.
#
# Env defaults use a per-leaf mkDefault, so a consumer that overrides one key
# keeps its siblings.

{ lib, ... }:

let
  # Upstream service addresses (the v0.8.3 compose service names).
  upstreams = {
    AUTH = { host = "auth"; port = "9999"; };
    REST = { host = "rest"; port = "3000"; };
    # NOTE: Realtime takes the tenant id from the first label of the Host
    # header, so the host and the Host rewrite below both start with the
    # seeded tenant name, realtime-dev.
    REALTIME = { host = "realtime-dev.supabase-realtime"; port = "4000"; };
    STORAGE = { host = "storage"; port = "5000"; };
    FUNCTIONS = { host = "functions"; port = "9000"; };
    META = { host = "meta"; port = "8080"; };
    STUDIO = { host = "studio"; port = "3000"; };
  };

  upstreamEnv = lib.concatMapAttrs (svc: a: {
    "ENVOY_${svc}_HOST" = a.host;
    "ENVOY_${svc}_PORT" = a.port;
  }) upstreams;
in
{
  config.envoy = {
    # NOTE: the version of `firestream.packages.envoy`, the release that
    # upstream v0.8.3 runs.
    version = lib.mkDefault "1.39.1";

    image.repository = lib.mkDefault "firestream-supabase-envoy";

    paths = {
      base = lib.mkDefault "/opt/firestream/envoy";
      conf = lib.mkDefault "/opt/firestream/envoy/config";
      data = lib.mkDefault "/firestream/envoy/data";
      logs = lib.mkDefault "/opt/firestream/envoy/logs";
    };

    env = builtins.mapAttrs (_: lib.mkDefault) ({
      ANON_KEY = "";
      SERVICE_ROLE_KEY = "";
      SUPABASE_PUBLISHABLE_KEY = "";
      SUPABASE_SECRET_KEY = "";
      ANON_KEY_ASYMMETRIC = "";
      SERVICE_ROLE_KEY_ASYMMETRIC = "";
      SUPABASE_PUBLIC_URL = "http://localhost:8000";
      DASHBOARD_USERNAME = "supabase";
      DASHBOARD_PASSWORD = "";

      ENVOY_HTTP_PORT = "8000";
      ENVOY_REALTIME_TENANT_HOST = "realtime-dev.supabase-realtime";
      # WARNING: the rendered files hold every API key. In Kubernetes mount an
      # emptyDir here; never a volume that outlives the pod.
      ENVOY_RUN_DIR = "/opt/firestream/envoy/run";
    } // upstreamEnv);

    envSecrets = lib.mkDefault [
      "ANON_KEY"
      "SERVICE_ROLE_KEY"
      "SUPABASE_PUBLISHABLE_KEY"
      "SUPABASE_SECRET_KEY"
      "ANON_KEY_ASYMMETRIC"
      "SERVICE_ROLE_KEY_ASYMMETRIC"
      "DASHBOARD_PASSWORD"
    ];

    exposedPorts = lib.mkDefault [ 8000 ];

    health = {
      enable = lib.mkDefault true;
      # The admin interface (127.0.0.1:9901, upstream envoy.yaml). It answers
      # 426 to HTTP/1.0, so the probe sends HTTP/1.1.
      readinessCmd = lib.mkDefault
        ''bash -c 'exec 3<>/dev/tcp/127.0.0.1/9901 && printf "GET /ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n" >&3 && head -n 1 <&3 | grep -q " 200"' '';
    };
  };
}
