# Supabase Realtime container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-realtime            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-realtime
#   - firestreamImages.supabase-realtime    (image triple for chart injection)
#   - checks.supabase-realtime-mix-deps     (mix-deps.nix matches mix.lock)
#
# The module builds the tailwind CLI for its assets with the vendored
# pnpm2nix, which reaches it through extraFactoryArgs.
{ ... }: {
  perSystem = { pkgs, lib, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/supabase-realtime/options.nix;
      modulePath = ../../../src/containers/firestream/supabase-realtime/module.nix;

      mkEval = extra: evalContainer {
        name = "realtime";
        runtimeType = "system";
        inherit modulePath;
        modules = [ optionsPath ] ++ extra;
        extraFactoryArgs.pnpm2nix = import ../../../bin/nix/firestream/node/pnpm2nix.nix { inherit pkgs; };
      };

      c = mkEval [ ];

      unavailable = n: pkgs.runCommand "${n}-not-available" { } ''
        echo "Docker images only available on Linux systems" > $out
      '';
    in
    {
      packages.supabase-realtime = if isLinux then c.dockerImage else unavailable "supabase-realtime";

      firestreamContainers.supabase-realtime = lib.optionalAttrs isLinux c;

      checks = lib.optionalAttrs isLinux {
        supabase-realtime-mix-deps = import ../../../bin/nix/firestream/beam/mix-deps-check.nix { inherit pkgs; } {
          name = "supabase-realtime-mix-deps";
          release = c.module.release;
          path = "src/containers/firestream/supabase-realtime";
        };
      };

      firestreamImages.supabase-realtime = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
