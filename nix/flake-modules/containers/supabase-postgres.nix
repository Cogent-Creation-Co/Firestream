# Supabase PostgreSQL container flake-module
# Copyright Firestream. MIT License.
#
# The postgresql container (version 17) with the Supabase options layer
# (src/containers/firestream/postgresql/supabase.nix). Contributes:
#   - packages.supabase-postgres            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-postgres
#   - firestreamImages.supabase-postgres    (image triple for chart injection)
#
# The container name stays "postgresql": the entrypoint, helper libraries and
# env prefix are the postgresql container's own, so the Bitnami-compatible
# scripts and the postgresql chart conventions apply unchanged.
{ ... }: {
  perSystem = { pkgs, lib, evalContainer, firestreamLib, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      containerDir = ../../../src/containers/firestream/postgresql;
      modulePath = containerDir + "/module.nix";

      modules = [
        (containerDir + "/options.nix")
        (import (containerDir + "/supabase.nix") { supabase = firestreamLib.supabase; })
        ({ ... }: { config.postgresql.version = "17"; })
      ];

      mkEval = extra: evalContainer {
        name = "postgresql";
        runtimeType = "system";
        inherit modulePath;
        modules = modules ++ extra;
      };

      c = mkEval [ ];

      unavailable = n: pkgs.runCommand "${n}-not-available" { } ''
        echo "Docker images only available on Linux systems" > $out
      '';
    in
    {
      packages.supabase-postgres = if isLinux then c.dockerImage else unavailable "supabase-postgres";

      firestreamContainers.supabase-postgres = lib.optionalAttrs isLinux c;

      firestreamImages.supabase-postgres = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
