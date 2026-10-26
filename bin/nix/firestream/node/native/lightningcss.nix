# The native binding of lightningcss, built from source
# Copyright Firestream. MIT License.
#
# @tailwindcss/node transforms CSS through lightningcss. The npm platform
# package holds only a prebuilt binary, so this derivation builds node/ of
# parcel-bundler/lightningcss at the tag of the same version, as
# scripts/build.js (`napi build --platform --cargo-cwd node --release`) does.
#
# Output: $out/lightningcss.linux-x64-gnu.node.

{ pkgs, fenix }:

let
  # NOTE: the toolchain that rust-toolchain.toml pins at the tag.
  toolchain = (fenix.fromManifestFile (builtins.fetchurl {
    url = "https://static.rust-lang.org/dist/channel-rust-1.92.0.toml";
    sha256 = "1l02hi83lj7v1536wgwnw430cc191705iharrzdfqwri6lj9d95j";
  })).minimalToolchain;
  rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
in
rustPlatform.buildRustPackage rec {
  pname = "lightningcss-node";
  version = "1.32.0";

  # NOTE: the commit of tag `v${version}`.
  src = pkgs.fetchFromGitHub {
    owner = "parcel-bundler";
    repo = "lightningcss";
    rev = "7f8a861bdee476fe90c89a8badeb3fd33a99c51a";
    hash = "sha256-JqAzBPYbJq52exCpVTdhd90BQGsbfMOx68PD1xUripg=";
  };

  # NOTE: cargoHash (a fetchCargoVendor vendor), not cargoLock.lockFile: a
  # Cargo.lock read from the fetched source at eval time is
  # import-from-derivation.
  cargoHash = "sha256-zndBkOJ/U6lLaOJn273TabMAPbz96hQeWs4cPKFDGro=";

  cargoBuildFlags = [ "-p" "lightningcss_node" ];
  doCheck = false;
  # NOTE: the module sits at the output root, which the default strip lists
  # miss; a full strip leaves the dynamic symbols that Node.js resolves.
  stripAllList = [ "." ];

  installPhase = ''
    runHook preInstall
    install -Dm555 target/*/release/liblightningcss_node.so $out/lightningcss.linux-x64-gnu.node
    runHook postInstall
  '';

  meta = {
    description = "The lightningcss CSS parser and minifier, as a Node-API module";
    homepage = "https://github.com/parcel-bundler/lightningcss";
    license = pkgs.lib.licenses.mpl20;
  };
}
