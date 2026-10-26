# Supabase Auth (GoTrue) Container Module
# Copyright Firestream. MIT License.
#
# GoTrue is a single Go binary. It reads GOTRUE_* environment variables and
# applies its own SQL migrations (in the `auth` schema) at every boot, so the
# image keeps no state.

{ pkgs
, lib
, firestream

, version ? "2.197.0"

, paths ? {
    base = "/opt/firestream/gotrue";
    conf = "/opt/firestream/gotrue/config";
    data = "/firestream/gotrue/data";
    logs = "/opt/firestream/gotrue/logs";
  }

, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 9999 ]
, imageName ? "firestream-supabase-auth"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  auth = firestream.packages.supabase-auth;
in
firestream.mkContainerModule {
  name = "gotrue";
  inherit version paths;
  inherit envVarsWithSecrets imageName imageTag exposedPorts health;

  envVars = {
    GOTRUE_DB_MIGRATIONS_PATH = "${auth}/share/auth/migrations";
  } // envVars;

  validateFn = ''
    [[ -n "''${GOTRUE_DB_DATABASE_URL:-}" ]] || print_validation_error "GOTRUE_DB_DATABASE_URL is required"
  '';

  configFn = ''
    if [[ -z "''${GOTRUE_JWT_ISSUER:-}" ]]; then
      export GOTRUE_JWT_ISSUER="$API_EXTERNAL_URL"
    fi
  '';

  runCmd = "auth";
  enableStateTracking = false;

  systemDeps = with pkgs; [ cacert coreutils ];
  runtimeBinDeps = [ auth ] ++ (with pkgs; [ coreutils bash gnugrep ]);

  user = { name = "gotrue"; group = "gotrue"; uid = 1001; gid = 1001; };
}
