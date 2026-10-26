# Supabase Storage container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-storage            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-storage
#   - firestreamImages.supabase-storage    (image triple for chart injection)
#
# The module builds the app with the vendored pnpm2nix and builds one
# WebAssembly module with a fenix toolchain, so both reach it through
# extraFactoryArgs.
{ inputs, ... }: {
  perSystem = { pkgs, lib, system, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/supabase-storage/options.nix;
      modulePath = ../../../src/containers/firestream/supabase-storage/module.nix;

      factoryArgs = {
        pnpm2nix = import ../../../bin/nix/firestream/node/pnpm2nix.nix { inherit pkgs; };
        fenix = inputs.fenix.packages.${system};
      };

      mkEval = extra: evalContainer {
        name = "storage";
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
      packages.supabase-storage = if isLinux then c.dockerImage else unavailable "supabase-storage";

      firestreamContainers.supabase-storage = lib.optionalAttrs isLinux c;

      firestreamImages.supabase-storage = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
