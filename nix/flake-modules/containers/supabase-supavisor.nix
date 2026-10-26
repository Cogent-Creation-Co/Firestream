# Supavisor container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-supavisor            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-supavisor
#   - firestreamImages.supabase-supavisor    (image triple for chart injection)
#   - checks.supabase-supavisor-mix-deps     (mix-deps.nix matches mix.lock)
{ ... }: {
  perSystem = { pkgs, lib, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/supavisor/options.nix;
      modulePath = ../../../src/containers/firestream/supavisor/module.nix;

      mkEval = extra: evalContainer {
        name = "supavisor";
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
      packages.supabase-supavisor = if isLinux then c.dockerImage else unavailable "supabase-supavisor";

      firestreamContainers.supabase-supavisor = lib.optionalAttrs isLinux c;

      checks = lib.optionalAttrs isLinux {
        supabase-supavisor-mix-deps = import ../../../bin/nix/firestream/beam/mix-deps-check.nix { inherit pkgs; } {
          name = "supabase-supavisor-mix-deps";
          release = c.module.release;
          path = "src/containers/firestream/supavisor";
        };
      };

      firestreamImages.supabase-supavisor = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
