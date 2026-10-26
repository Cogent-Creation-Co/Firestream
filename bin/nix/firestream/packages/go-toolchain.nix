# Go toolchain for the Supabase Go services
# Copyright Firestream. MIT License.
#
# supabase/auth v2.197.0 requires go >= 1.27 and imgproxy v3.31.4 requires
# go >= 1.26; the pinned nixpkgs ships go 1.25. The pin's go_1_25 expression
# builds the newer source tree unchanged (same patches), bootstrapped by
# go_1_25 itself.
#
# Usage: buildGoModule.override { go = <this>.go; } via `buildGoModule`.

{ pkgs, lib }:

let
  go = pkgs.go_1_25.overrideAttrs (old: rec {
    version = "1.27.3";
    # NOTE: the git tree of tag go1.27.3, not the go.dev release tarball.
    # The release commit holds the VERSION file, so the tree builds as is.
    src = pkgs.fetchFromGitHub {
      owner = "golang";
      repo = "go";
      rev = "2aae1000ea1d4b28923b96db28acf4e3b99d6aff";
      hash = "sha256-9sYPwyHHgDLFYbLC26tQeiIzhUzji0IScE1FQ4Fxi44=";
    };
    patches = map
      (p: if lib.hasSuffix "go_no_vendor_checks-1.23.patch" (toString p)
          then ./go-no-vendor-checks-1.27.patch else p)
      old.patches;
    GOROOT_BOOTSTRAP = "${pkgs.go_1_25}/share/go";
    disallowedReferences = [ pkgs.go_1_25 ];
  });
in
{
  inherit go;
  buildGoModule = pkgs.buildGoModule.override { inherit go; };
}
