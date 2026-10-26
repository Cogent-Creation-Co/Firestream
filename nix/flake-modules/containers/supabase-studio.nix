# Supabase Studio container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-studio            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-studio
#   - firestreamImages.supabase-studio    (image triple for chart injection)
#
# The module builds the app with the vendored pnpm2nix and builds its
# native build tools with fenix toolchains, so both reach it through
# extraFactoryArgs.
{ inputs, ... }: {
  perSystem = { pkgs, lib, system, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/supabase-studio/options.nix;
      modulePath = ../../../src/containers/firestream/supabase-studio/module.nix;

      factoryArgs = {
        pnpm2nix = import ../../../bin/nix/firestream/node/pnpm2nix.nix { inherit pkgs; };
        fenix = inputs.fenix.packages.${system};
      };

      mkEval = extra: evalContainer {
        name = "studio";
        runtimeType = "system";
        inherit modulePath;
        modules = [ optionsPath ] ++ extra;
        extraFactoryArgs = factoryArgs;
      };

      c = mkEval [ ];

      unavailable = n: pkgs.runCommand "${n}-not-available" { } ''
        echo "Docker images only available on Linux systems" > $out
      '';
    in
    {
      packages.supabase-studio = if isLinux then c.dockerImage else unavailable "supabase-studio";

      firestreamContainers.supabase-studio = lib.optionalAttrs isLinux c;

      firestreamImages.supabase-studio = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
