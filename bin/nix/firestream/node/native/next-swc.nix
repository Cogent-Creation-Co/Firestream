# The native binding of Next.js (@next/swc-linux-x64-gnu), built from source
# Copyright Firestream. MIT License.
#
# `next build` loads @next/swc for SWC transforms and Turbopack. The npm
# platform package holds only a prebuilt binary, so this derivation builds
# crates/next-napi-bindings of vercel/next.js at the tag of the same
# version, with the features of `build-native-release` in
# packages/next-swc/package.json.
#
# Output: $out/next-swc.linux-x64-gnu.node.

{ pkgs, fenix }:

let
  version = "16.3.6";
  rev = "a758ffcf501f6f1ddb03175bd1033508424c261e";

  # NOTE: the nightly that rust-toolchain.toml pins at `rev`; the workspace
  # uses -Zshare-generics and other unstable flags in .cargo/config.toml.
  toolchain = (fenix.fromManifestFile (builtins.fetchurl {
    url = "https://static.rust-lang.org/dist/2026-07-29/channel-rust-nightly.toml";
    sha256 = "13y03phwaq0593bxx397bi002d1g0m4w7qnd5j9nbhv33nnr6q9y";
  })).minimalToolchain;
  rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
in
rustPlatform.buildRustPackage {
  pname = "next-swc";
  inherit version;

  # NOTE: the commit of tag `v${version}`.
  src = pkgs.fetchFromGitHub {
    owner = "vercel";
    repo = "next.js";
    inherit rev;
    hash = "sha256-IkZuRVG0oGvKIZxmA1FwDnLJfCi/1VQCPkEuENqg/IE=";
  };

  # NOTE: cargoHash (a fetchCargoVendor vendor), not cargoLock.lockFile: a
  # Cargo.lock read from the fetched source at eval time is
  # import-from-derivation.
  cargoHash = "sha256-oOPEUripVKejDmCkVbDzQirkhDtbripg0nr4XKnz0II=";

  cargoBuildFlags = [
    "-p" "next-napi-bindings"
    "--features" "image-extended,tracing/release_max_level_trace"
  ];

  # NOTE: build.rs asks a git work tree for these two values through
  # vergen-gitcl, and the source tarball has none. postPatch drops that
  # step, and rustc reads the values from this environment.
  env = {
    VERGEN_GIT_SHA = builtins.substring 0 7 rev;
    VERGEN_GIT_DIRTY = "false";
  };

  # NOTE: the include_dir fork in the Cargo.lock takes its crate docs from
  # ../../README.md, a file of its git workspace that the vendor of one
  # crate does not copy.
  postPatch = ''
    chmod u+w "$cargoDepsCopy"
    touch "$cargoDepsCopy"/README.md

    substituteInPlace crates/next-napi-bindings/build.rs \
      --replace-fail ".add_instructions(&git)?" ""
  '';

  nativeBuildInputs = [ pkgs.cmake pkgs.perl pkgs.python3 ];
  dontUseCmakeConfigure = true;
  doCheck = false;
  # NOTE: the module sits at the output root, which the default strip lists
  # miss; a full strip leaves the dynamic symbols that Node.js resolves.
  stripAllList = [ "." ];

  installPhase = ''
    runHook preInstall
    install -Dm555 target/*/release/libnext_napi_bindings.so $out/next-swc.linux-x64-gnu.node
    runHook postInstall
  '';

  meta = {
    description = "The SWC and Turbopack native binding of Next.js";
    homepage = "https://github.com/vercel/next.js";
    license = pkgs.lib.licenses.mit;
  };
}
