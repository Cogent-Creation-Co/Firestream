{
  rustPlatform,
  lib,
}:

# Drop-in path for callPackage-style consumers. The flake.nix wires this with
# crane (mirroring src/util/otel-cli and src/util/firestream-nix-build) so that
# the unified workspace is built without disturbing the outer monorepo
# workspace. This file is a fallback that uses rustPlatform for environments
# where crane is unavailable.
rustPlatform.buildRustPackage {
  pname = "firestream-ci";
  version = "0.1.0";
  src = lib.cleanSource ../.;

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  buildAndTestSubdir = "firestream-ci";
  doCheck = false;

  meta = {
    description = "Build-pipeline primitives: composes otel-cli + firestream-nix-build into a reusable Nix + Docker + OTel toolkit";
    homepage = "https://github.com/Cogent-Creation-Co/Firestream";
    license = lib.licenses.asl20;
    mainProgram = "firestream-ci";
  };
}
