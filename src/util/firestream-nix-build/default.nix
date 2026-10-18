{
  rustPlatform,
  lib,
  nix-eval-jobs,
  nix-output-monitor,
  makeWrapper,
}:

# Drop-in path for callPackage-style consumers. The flake.nix wires this with
# crane (mirroring src/util/otel-cli) so that the isolated workspace is built
# without disturbing the outer monorepo workspace.
rustPlatform.buildRustPackage {
  pname = "firestream-nix-build";
  version = "0.1.0";
  src = lib.cleanSource ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  nativeBuildInputs = [ makeWrapper ];

  postFixup = ''
    wrapProgram $out/bin/firestream-nix-build \
      --prefix PATH : ${lib.makeBinPath [ nix-eval-jobs nix-eval-jobs.nix nix-output-monitor ]}
  '';

  doCheck = false;

  meta = {
    description = "Rust port of nix-fast-build with in-process OpenTelemetry ingest (PRD §9.2)";
    homepage = "https://github.com/Cogent-Creation-Co/Firestream";
    license = lib.licenses.asl20;
    mainProgram = "firestream-nix-build";
  };
}
