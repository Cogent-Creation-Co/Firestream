# cargo-bazel, the crate_universe generator of rules_rust.
# Copyright Firestream. MIT License.
#
# `crates_repository` runs this binary to render the crate BUILD files
# from Envoy's Cargo.Bazel.lock. rules_rust would download a prebuilt one.

{ pkgs, rulesRust }:

pkgs.rustPlatform.buildRustPackage {
  pname = "cargo-bazel";
  version = "0.17.0";

  src = rulesRust;
  sourceRoot = ".";
  cargoRoot = "crate_universe";
  buildAndTestSubdir = "crate_universe";

  # NOTE: a copy of `crate_universe/Cargo.lock` from the same archive. Nix
  # reads it at evaluation time; a path inside `src` would need
  # import-from-derivation. The build fails if the two copies differ.
  cargoLock.lockFile = ./cargo-bazel.Cargo.lock;

  cargoBuildFlags = [ "--bin" "cargo-bazel" ];
  doCheck = false;

  meta.mainProgram = "cargo-bazel";
}
