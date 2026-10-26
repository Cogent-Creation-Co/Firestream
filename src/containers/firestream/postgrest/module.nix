# PostgREST Container Module
# Copyright Firestream. MIT License.
#
# PostgREST is a single binary that reads all of its configuration from
# PGRST_* environment variables. The image keeps no state, so it runs with a
# read-only root filesystem.

{ pkgs
, lib
, firestream

, version ? "14.1"

, paths ? {
    base = "/opt/firestream/postgrest";
    conf = "/opt/firestream/postgrest/config";
    data = "/firestream/postgrest/data";
    logs = "/opt/firestream/postgrest/logs";
  }

, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 3000 ]
, imageName ? "firestream-supabase-postgrest"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

firestream.mkContainerModule {
  name = "postgrest";
  inherit version paths envVars envVarsWithSecrets;
  inherit imageName imageTag exposedPorts health;

  validateFn = ''
    [[ -n "''${PGRST_DB_URI:-}" ]] || print_validation_error "PGRST_DB_URI is required"
  '';

  runCmd = "postgrest";
  enableStateTracking = false;

  systemDeps = with pkgs; [ cacert coreutils ];
  runtimeBinDeps = [ pkgs.postgrest ] ++ (with pkgs; [ coreutils bash gnugrep ]);

  user = { name = "postgrest"; group = "postgrest"; uid = 1001; gid = 1001; };
}
