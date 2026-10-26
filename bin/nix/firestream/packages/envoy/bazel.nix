# Bazel at the version Envoy's .bazelversion names.
# Copyright Firestream. MIT License.
#
# The pinned nixpkgs ships bazel_8 8.4.2. Its expression builds the 8.7.0
# distribution archive unchanged.

{ pkgs }:

let
  version = "8.7.0";
in
(pkgs.bazel_8.override { inherit version; }).overrideAttrs {
  # NOTE: the `-dist.zip` is Bazel's bootstrap source archive: the Java and
  # C++ sources plus the source archives of its own dependencies. The git
  # tree cannot bootstrap without a Bazel binary.
  src = pkgs.fetchzip {
    url = "https://github.com/bazelbuild/bazel/releases/download/${version}/bazel-${version}-dist.zip";
    hash = "sha256-bm+gkkrAVepJWZddDEV1+jX4jRdD+tZ1TH2YG9adIyk=";
    stripRoot = false;
  };
}
