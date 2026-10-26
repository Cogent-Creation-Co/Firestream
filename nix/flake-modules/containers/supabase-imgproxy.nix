# imgproxy container flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-imgproxy            (docker image; stub off-Linux)
#   - firestreamContainers.supabase-imgproxy
#   - firestreamImages.supabase-imgproxy    (image triple for chart injection)
{ ... }: {
  perSystem = { pkgs, lib, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/imgproxy/options.nix;
      modulePath = ../../../src/containers/firestream/imgproxy/module.nix;

      mkEval = extra: evalContainer {
        name = "imgproxy";
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
      packages.supabase-imgproxy = if isLinux then c.dockerImage else unavailable "supabase-imgproxy";

      firestreamContainers.supabase-imgproxy = lib.optionalAttrs isLinux c;

      firestreamImages.supabase-imgproxy = {
        dockerImage = c.dockerImage;
        eval = userMod: mkEval [ userMod ];
        options = c.options;
      };
    };
}
