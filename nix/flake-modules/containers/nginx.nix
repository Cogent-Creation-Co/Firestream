# Nginx container flake-module
# Copyright Firestream. MIT License.
#
# Wires the nginx container (single-version reverse proxy) through the
# options-driven evalContainer entrypoint and contributes:
#   - packages.nginx             (docker image; stub off-Linux)
#   - firestreamContainers.nginx
#   - firestreamImages.nginx     (consumer override API + compose; the chart
#                                 flake-module reads .eval for the image triple)
#
# runtimeType "system". nginx is NOT a Bitnami chart, so the container carries
# no Bitnami-compat scripts and needs no extraFactoryArgs - `nginx` comes
# straight from nixpkgs via runtimeBinDeps (see src/containers/firestream/nginx).
{ ... }: {
  perSystem = { pkgs, lib, system, evalContainer, ... }:
    let
      isLinux = pkgs.stdenv.hostPlatform.isLinux;

      optionsPath = ../../../src/containers/firestream/nginx/options.nix;
      modulePath = ../../../src/containers/firestream/nginx/module.nix;

      c = evalContainer {
        name = "nginx";
        runtimeType = "system";
        inherit modulePath;
        modules = [ optionsPath ];
      };

      unavailable = n: pkgs.runCommand "${n}-not-available" { } ''
        echo "Docker images only available on Linux systems" > $out
      '';
    in
    {
      packages.nginx = if isLinux then c.dockerImage else unavailable "nginx";

      firestreamContainers.nginx = lib.optionalAttrs isLinux c;

      firestreamImages.nginx = {
        dockerImage = c.dockerImage;
        eval = userMod: evalContainer {
          name = "nginx";
          runtimeType = "system";
          inherit modulePath;
          modules = [ optionsPath userMod ];
        };
        options = c.options;
      };
    };
}
