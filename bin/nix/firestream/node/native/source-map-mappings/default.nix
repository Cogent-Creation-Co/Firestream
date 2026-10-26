# lib/mappings.wasm of the `source-map` npm package, built from source
# Copyright Firestream. MIT License.
#
# The npm tarball of source-map 0.7.6 ships lib/mappings.wasm prebuilt. Its
# Rust source is wasm-mappings/source-map-mappings-wasm-api in
# mozilla/source-map at the tag of the same version. Upstream commits no
# Cargo.lock for it; ./Cargo.lock locks its crates.io dependencies.
#
# Output: $out/mappings.wasm, a drop-in for lib/mappings.wasm.

{ pkgs, fenix }:

let
  # NOTE: Rust 1.65, the stable release when upstream built the npm file.
  # `generated_location_for` takes the first `binary_search_by` hit among
  # mappings with equal original positions, so the std search algorithm
  # picks which tie a lookup returns. 1.65 returns the same mappings as the
  # npm file on every source map of Storage; a current compiler does not.
  manifest = builtins.fetchurl {
    url = "https://static.rust-lang.org/dist/channel-rust-1.65.0.toml";
    sha256 = "0mj653fky0afjnrwkpch6i5fcl81g7dy4f6g0wzyzq7ndrll8cqg";
  };
  rust = fenix.fromManifestFile manifest;
  toolchain = fenix.combine [
    rust.cargo
    rust.rustc
    (fenix.targets.wasm32-unknown-unknown.fromManifestFile manifest).rust-std
  ];
in
pkgs.stdenv.mkDerivation {
  pname = "source-map-mappings-wasm";
  version = "0.7.6";

  # NOTE: the commit of tag `0.7.6`.
  src = pkgs.fetchFromGitHub {
    owner = "mozilla";
    repo = "source-map";
    rev = "801be934007c3ed0ef66c620641b1668e92c891d";
    hash = "sha256-bADStCQj6fmfjwyOyTjBVZ8UrweU94hiunBdIjDnmK0=";
  };
  sourceRoot = "source/wasm-mappings/source-map-mappings-wasm-api";

  cargoDeps = pkgs.rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };
  postPatch = "cp ${./Cargo.lock} Cargo.lock";

  nativeBuildInputs = [ toolchain pkgs.rustPlatform.cargoSetupHook pkgs.binaryen ];

  # NOTE: --allow-undefined, because the module imports `mapping_callback`
  # from the JavaScript host, and rust-lld rejects an undefined symbol.
  buildPhase = ''
    runHook preBuild
    RUSTFLAGS="-C link-arg=--allow-undefined" \
      cargo build --offline --release --target wasm32-unknown-unknown
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    wasm-opt -O3 --strip-debug \
      target/wasm32-unknown-unknown/release/source_map_mappings_wasm_api.wasm \
      -o $out/mappings.wasm
    runHook postInstall
  '';

  meta = {
    description = "The WebAssembly mappings parser of the source-map npm package";
    homepage = "https://github.com/mozilla/source-map";
    license = pkgs.lib.licenses.bsd3;
  };
}
