# PostgREST container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-postgrest            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-postgrest
#   - firestreamImages.supabase-postgrest    (image triple for chart injection)
{ ... }: {
  perSystem = { pkgs, lib, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/postgrest/options.nix;
      modulePath = ../../../src/containers/firestream/postgrest/module.nix;

      mkEval = extra: evalContainer {
        name = "postgrest";
        runtimeType = "system";
        inherit modulePath;
        modules = [ optionsPath ] ++ extra;
      };

      c = mkEval [ ];

      unavailable = n: pkgs.runCommand "${n}-not-available" { } ''
        echo "Docker images only available on Linux systems" > $out
      '';
    in
    {
      packages.supabase-postgrest = if isLinux then c.dockerImage else unavailable "supabase-postgrest";

      firestreamContainers.supabase-postgrest = lib.optionalAttrs isLinux c;

      firestreamImages.supabase-postgrest = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
