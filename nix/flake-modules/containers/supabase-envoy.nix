# Supabase API gateway (Envoy) container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-envoy            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-envoy
#   - firestreamImages.supabase-envoy    (image triple for chart injection)
#   - checks.supabase-envoy-deps         (envoy/deps.json matches a fresh
#                                         generate-deps.py run)
#
# compose.nix declares the whole Supabase stack on this image, so
# packages.supabase-envoy-compose is that stack (see nix/flake-modules/supabase.nix).
{ ... }: {
  perSystem = { pkgs, lib, evalContainer, firestreamLib, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/supabase-envoy/options.nix;
      modulePath = ../../../src/containers/firestream/supabase-envoy/module.nix;
      composePath = ../../../src/containers/firestream/supabase-envoy/compose.nix;

      mkEval = extra: evalContainer {
        name = "envoy";
        runtimeType = "system";
        inherit modulePath;
        modules = [
          optionsPath
          (import composePath { inherit (firestreamLib.supabase) devSecrets; })
        ] ++ extra;
      };

      c = mkEval [ ];

      unavailable = n: pkgs.runCommand "${n}-not-available" { } ''
        echo "Docker images only available on Linux systems" > $out
      '';
    in
    {
      packages.supabase-envoy = if isLinux then c.dockerImage else unavailable "supabase-envoy";

      firestreamContainers.supabase-envoy = lib.optionalAttrs isLinux c;

      checks = lib.optionalAttrs isLinux {
        supabase-envoy-deps = firestreamLib.packages.envoy.depsCheck;
      };

      firestreamImages.supabase-envoy = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
