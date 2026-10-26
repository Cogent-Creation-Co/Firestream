# imgproxy Nix package
# Copyright Firestream. MIT License.
#
# The nixpkgs imgproxy expression at the version that Supabase self-hosted
# v0.8.3 runs (darthsim/imgproxy:v3.31.4). Open-source build only: imgproxy
# Pro features are commercial and are not in this source tree.

{ pkgs, lib, goToolchain }:

(pkgs.imgproxy.override { inherit (goToolchain) buildGoModule; }).overrideAttrs (old: rec {
  version = "3.31.4";

  src = pkgs.fetchFromGitHub {
    owner = "imgproxy";
    repo = "imgproxy";
    rev = "8b0aa78279f191081054ba709b39f317f66ace78";
    hash = "sha256-eFx0UMJKs6xmcqW3MRfXyeVfGZ1kjYGqSncUni7VTiA=";
  };

  vendorHash = "sha256-mfGaGEK4nUFMDtVVMeqTOR2MPURBlocr30iJ+xXIhFY=";
})
