# Postgres extensions that nixpkgs does not package
# Copyright Firestream. MIT License.
#
# Returns a function from a nixpkgs `postgresql_<N>` package to the extension
# set built against it:
#
#   (import ./postgresql-extensions { inherit pkgs lib; }) pkgs.postgresql_17
#   => { supautils; supabase_vault; plan_filter; }
#
# Each attribute name equals the extension's library name, so the set merges
# into `postgresql.pkgs` for `postgresql.withPackages`.

{ pkgs, lib }:

postgresql:

let
  pgDir = pkgs.path + "/pkgs/servers/sql/postgresql";

  # NOTE: nixpkgs builds extensions in a private scope and does not export
  # postgresqlBuildExtension. The scope must use `postgresql.stdenv`: the pg17
  # stdenv is clang, and PGXS fails under the default gcc stdenv with
  # `clang: command not found`.
  scope = {
    inherit postgresql;
    inherit (postgresql) stdenv;
    postgresqlBuildExtension = pkgs.callPackage (pgDir + "/postgresqlBuildExtension.nix") {
      inherit postgresql;
      inherit (postgresql) stdenv;
    };
  };

  callPackage = lib.callPackageWith (pkgs // scope);
in
{
  supautils = callPackage ./supautils.nix { };
  supabase_vault = callPackage ./supabase-vault.nix { };
  plan_filter = callPackage ./plan-filter.nix { };
}
