# Supabase Auth (GoTrue) container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-auth            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-auth
#   - firestreamImages.supabase-auth    (image triple for chart injection)
{ ... }: {
  perSystem = { pkgs, lib, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/supabase-auth/options.nix;
      modulePath = ../../../src/containers/firestream/supabase-auth/module.nix;

      mkEval = extra: evalContainer {
        name = "gotrue";
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
      packages.supabase-auth = if isLinux then c.dockerImage else unavailable "supabase-auth";

      firestreamContainers.supabase-auth = lib.optionalAttrs isLinux c;

      firestreamImages.supabase-auth = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
