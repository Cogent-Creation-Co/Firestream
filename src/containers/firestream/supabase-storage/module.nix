# Supabase Storage Container Module
# Copyright Firestream. MIT License.
#
# A Node.js API: ./package.nix builds it with pnpm2nix, and
# mkNodeContainerModule wraps it. Object bytes go to the S3 backend
# (SeaweedFS); metadata goes to the `storage` schema, which Storage migrates
# at every start. The image keeps no state.
#
# pnpm2nix (the patched builder) and fenix come from the flake-module
# through extraFactoryArgs.

{ pkgs
, lib
, firestream
, pnpm2nix
, fenix
, version ? "1.81.0"
, paths ? {
    base = "/opt/firestream/storage";
    conf = "/opt/firestream/storage/config";
    data = "/firestream/storage/data";
    logs = "/opt/firestream/storage/logs";
  }
, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 5000 ]
, imageName ? "firestream-supabase-storage"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  storage = import ./package.nix { inherit pkgs lib pnpm2nix fenix version; };
in
firestream.mkNodeContainerModule {
  name = "storage";
  inherit version paths;
  inherit envVarsWithSecrets exposedPorts imageName imageTag health;
  # Storage reports VERSION as its appVersion in logs and metrics.
  envVars = { VERSION = version; } // envVars;
  inherit (storage) nodejs;
  nodePackage = storage;

  validateFn = ''
    for var in DATABASE_URL ANON_KEY SERVICE_KEY AUTH_JWT_SECRET; do
      [[ -n "''${!var:-}" ]] || print_validation_error "$var is required"
    done
  '';

  runCmd = "storage-api";

  # The health.readinessCmd tools; healthd runs with the image PATH.
  systemDeps = with pkgs; [ coreutils gnugrep ];

  enableStateTracking = false;

  user = { name = "storage"; group = "storage"; uid = 1001; gid = 1001; };
}
