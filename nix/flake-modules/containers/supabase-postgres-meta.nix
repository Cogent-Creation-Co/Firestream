# postgres-meta container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-postgres-meta            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-postgres-meta
#   - firestreamImages.supabase-postgres-meta    (image triple for chart injection)
#
# The module builds the app with the vendored pnpm2nix and builds one
# native binding with a fenix toolchain, so both reach it through
# extraFactoryArgs.
{ inputs, ... }: {
  perSystem = { pkgs, lib, system, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/postgres-meta/options.nix;
      modulePath = ../../../src/containers/firestream/postgres-meta/module.nix;

      factoryArgs = {
        pnpm2nix = import ../../../bin/nix/firestream/node/pnpm2nix.nix { inherit pkgs; };
        fenix = inputs.fenix.packages.${system};
      };

      mkEval = extra: evalContainer {
        name = "pgmeta";
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
      packages.supabase-postgres-meta = if isLinux then c.dockerImage else unavailable "supabase-postgres-meta";

      firestreamContainers.supabase-postgres-meta = lib.optionalAttrs isLinux c;

      firestreamImages.supabase-postgres-meta = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
