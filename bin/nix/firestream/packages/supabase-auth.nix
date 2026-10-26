# supabase-auth Nix package
# Copyright Firestream. MIT License.
#
# Supabase Auth (GoTrue) at the version that Supabase self-hosted v0.8.3 runs
# (supabase/gotrue:v2.197.0). Not the nixpkgs `gotrue` (the Netlify fork) nor
# `gotrue-supabase` (an older release).
#
# The SQL migrations ship in `$out/share/auth/migrations`; the binary applies
# them at boot from GOTRUE_DB_MIGRATIONS_PATH.

{ pkgs, lib, goToolchain }:

goToolchain.buildGoModule rec {
  pname = "supabase-auth";
  version = "2.197.0";

  src = pkgs.fetchFromGitHub {
    owner = "supabase";
    repo = "auth";
    rev = "4eee58f296d9698a1c2c0ae14d7a0b379c7622d3";
    hash = "sha256-JDcqCJkZmEXTdXrD1HXuKEwqR7F+PCLNSzw6+V9xkRk=";
  };

  vendorHash = "sha256-MIjxvI5DTjuYvZCKDiNF0zLu6ovimeiM043uxmRH5hM=";

  subPackages = [ "." ];
  env.CGO_ENABLED = "0";
  ldflags = [ "-s" "-w" "-X github.com/supabase/auth/internal/utilities.Version=v${version}" ];

  # NOTE: the upstream test suite needs a live Postgres.
  doCheck = false;

  postInstall = ''
    ln -s auth $out/bin/gotrue
    mkdir -p $out/share/auth
    cp -r migrations $out/share/auth/migrations
  '';

  meta = {
    description = "Supabase Auth: JWT-based user management API";
    homepage = "https://github.com/supabase/auth";
    license = lib.licenses.mit;
    mainProgram = "auth";
  };
}
