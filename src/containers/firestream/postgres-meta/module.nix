# postgres-meta Container Module
# Copyright Firestream. MIT License.
#
# A stateless Node.js API: ./package.nix builds it with pnpm2nix, and
# mkNodeContainerModule wraps it. It connects to Postgres with the
# PG_META_DB_* settings, or with an encrypted connection string that a
# client (Studio) sends per request.
#
# pnpm2nix (the patched builder) and the fenix packages come from the
# flake-module through extraFactoryArgs.

{ pkgs
, lib
, firestream
, pnpm2nix
, fenix
, version ? "0.100.0"
, paths ? {
    base = "/opt/firestream/pgmeta";
    conf = "/opt/firestream/pgmeta/config";
    data = "/firestream/pgmeta/data";
    logs = "/opt/firestream/pgmeta/logs";
  }
, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 8080 ]
, imageName ? "firestream-supabase-postgres-meta"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  pgMeta = import ./package.nix { inherit pkgs lib pnpm2nix fenix version; };
in
firestream.mkNodeContainerModule {
  name = "pgmeta";
  inherit version paths;
  inherit envVars envVarsWithSecrets exposedPorts imageName imageTag health;
  inherit (pgMeta) nodejs;
  nodePackage = pgMeta;

  validateFn = ''
    [[ -n "''${PG_META_DB_PASSWORD:-}" ]] || print_validation_error "PG_META_DB_PASSWORD is required"
    [[ -n "''${CRYPTO_KEY:-}" ]] || print_validation_error "CRYPTO_KEY is required"
  '';

  runCmd = "postgres-meta";

  # The health.readinessCmd tools; healthd runs with the image PATH.
  systemDeps = with pkgs; [ coreutils gnugrep ];

  enableStateTracking = false;

  user = { name = "pgmeta"; group = "pgmeta"; uid = 1001; gid = 1001; };
}
