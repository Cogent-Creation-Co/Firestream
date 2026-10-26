# The native binding of Tailwind CSS 4 (@tailwindcss/oxide), built from source
# Copyright Firestream. MIT License.
#
# @tailwindcss/postcss scans the sources for class names through
# @tailwindcss/oxide. The npm platform package holds only a prebuilt
# binary, so this derivation builds crates/node of tailwindlabs/tailwindcss
# at the tag of the same version, as `napi build --platform --release` does.
#
# Output: $out/tailwindcss-oxide.linux-x64-gnu.node.

{ pkgs, fenix }:

let
  # NOTE: the toolchain that rust-toolchain.toml pins at the tag.
  toolchain = (fenix.fromManifestFile (builtins.fetchurl {
    url = "https://static.rust-lang.org/dist/channel-rust-1.85.0.toml";
    sha256 = "19ax822lxxl271i0qi9a0rr30icnkzc9ynsy9djby4izyigqp7h0";
  })).minimalToolchain;
  rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
in
rustPlatform.buildRustPackage rec {
  pname = "tailwindcss-oxide";
  version = "4.2.4";

  # NOTE: the commit of tag `v${version}`.
  src = pkgs.fetchFromGitHub {
    owner = "tailwindlabs";
    repo = "tailwindcss";
    rev = "69ad7cc5ec091125842aba783c5bfce536e9c234";
    hash = "sha256-IwLme3q/5NDxNn7QgXDHbaXYpvOm/6cZLUS+3vZD6yQ=";
  };

  # NOTE: cargoHash (a fetchCargoVendor vendor), not cargoLock.lockFile: a
  # Cargo.lock read from the fetched source at eval time is
  # import-from-derivation.
  cargoHash = "sha256-VjsjM5HV+5JmYH9w7EF7b78tGHs8fF7BhtTpsftN0nQ=";

  cargoBuildFlags = [ "-p" "tailwind-oxide" ];
  doCheck = false;
  # NOTE: the module sits at the output root, which the default strip lists
  # miss; a full strip leaves the dynamic symbols that Node.js resolves.
  stripAllList = [ "." ];

  installPhase = ''
    runHook preInstall
    install -Dm555 target/*/release/libtailwind_oxide.so $out/tailwindcss-oxide.linux-x64-gnu.node
    runHook postInstall
  '';

  meta = {
    description = "The Rust core of Tailwind CSS 4, as a Node-API module";
    homepage = "https://github.com/tailwindlabs/tailwindcss";
    license = pkgs.lib.licenses.mit;
  };
}
