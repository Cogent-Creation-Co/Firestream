# The WebAssembly parser of the `libpg-query` npm package, built from source
# Copyright Firestream. MIT License.
#
# The npm tarball of libpg-query 17.6.0 ships wasm/libpg-query.wasm and its
# Emscripten loader wasm/libpg-query.js prebuilt. This derivation builds
# both from pganalyze/libpg_query and from versions/17 of
# constructive-io/libpg-query-node with the nixpkgs Emscripten. Replace the
# two files together: the loader and the module come from one Emscripten
# run, and a loader from another Emscripten version does not match.
#
# Output: $out/libpg-query.{js,wasm}, drop-ins for the files in wasm/.

{ pkgs }:

let
  # NOTE: the commit of tag `17-6.1.0`, the LIBPG_QUERY_TAG in the
  # versions/17 Makefile.
  libpgQuery = pkgs.fetchFromGitHub {
    owner = "pganalyze";
    repo = "libpg_query";
    rev = "1c1a32ed2f4c7799830d50bf4cb159222aafec48";
    hash = "sha256-UXba2WYyIO7RcFcNZeLL+Q9CwlloMZ5oFfHfL7+j4dU=";
  };
in
pkgs.stdenv.mkDerivation {
  pname = "libpg-query-wasm";
  version = "17.6.0";

  # NOTE: the head of branch fix/increase-memory, which npm published as
  # 17.6.0 three minutes later. The npm gitHead (7a7825a4) is a local
  # version-bump commit that GitHub does not hold.
  src = pkgs.fetchFromGitHub {
    owner = "constructive-io";
    repo = "libpg-query-node";
    rev = "727c0826216a64a417941ead4e4c5d6af77d652c";
    hash = "sha256-Jw2PEEXxpOodfeKY8GFEWTAOagLSEsEwRHxMt87Sfzw=";
  };
  sourceRoot = "source/versions/17";

  nativeBuildInputs = [ pkgs.emscripten pkgs.python3 pkgs.which ];

  # NOTE: the Makefile clones libpg_query into this directory when it is
  # absent, and the sandbox has no network.
  configurePhase = ''
    runHook preConfigure
    export HOME=$TMPDIR
    export EM_CACHE=$TMPDIR/emscripten-cache
    cp -r --no-preserve=mode ${pkgs.emscripten}/share/emscripten/cache $EM_CACHE
    mkdir -p .cache/emscripten-wasm/libpg_query
    cp -r --no-preserve=mode ${libpgQuery} .cache/emscripten-wasm/libpg_query/17-6.1.0
    runHook postConfigure
  '';

  buildPhase = ''
    runHook preBuild
    EMSCRIPTEN=1 emmake make -j$NIX_BUILD_CORES build-cache
    # WARNING: no -j here. The .js and the .wasm are two targets of one
    # recipe; in parallel, make links the .wasm alone as a standalone
    # module, which fails for want of `main`.
    EMSCRIPTEN=1 emmake make build
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm444 wasm/libpg-query.js $out/libpg-query.js
    install -Dm444 wasm/libpg-query.wasm $out/libpg-query.wasm
    runHook postInstall
  '';

  meta = {
    description = "The PostgreSQL 17 query parser of libpg_query, compiled to WebAssembly";
    homepage = "https://github.com/constructive-io/libpg-query-node";
    license = pkgs.lib.licenses.mit;
  };
}
