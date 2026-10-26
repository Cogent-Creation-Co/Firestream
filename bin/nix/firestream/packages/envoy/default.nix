# Envoy, built offline by Bazel from per-dependency pins.
# Copyright Firestream. MIT License.
#
# deps.json (written by generate-deps.py from the Envoy tree) lists every
# archive that //source/exe:envoy-static needs. Each one is a fetchurl with
# the sha256 that Envoy declares, and a linkFarm lays them out as a Bazel
# repository cache. Bazel then runs with downloads disabled. The prebuilt
# toolchains that Envoy would download are replaced by nixpkgs and fenix
# ones; deps.json names each replacement under `excluded`.

{ pkgs, lib, fenix, system }:

let
  version = "1.39.1";
  # v1.39.1
  rev = "b579d07d3ad7ee11d32b105e91a5a39ad24718d7";

  deps = lib.importJSON ./deps.json;

  upstream = pkgs.fetchFromGitHub {
    owner = "envoyproxy";
    repo = "envoy";
    inherit rev;
    hash = "sha256-V+J/eTWzWiLqL70O4tqGagNFeDxpA1YGhYklF57IBEo=";
  };

  bazel = import ./bazel.nix { inherit pkgs; };

  python = pkgs.python312;

  fetchArchive = a: pkgs.fetchurl {
    name = lib.strings.sanitizeDerivationName "${a.name}-${baseNameOf (builtins.head a.urls)}";
    inherit (a) urls sha256;
  };

  archive = name: fetchArchive (lib.findFirst (a: a.name == name)
    (throw "envoy: ${name} is not in deps.json")
    deps.archives);

  repositoryCache = pkgs.linkFarm "envoy-${version}-repository-cache"
    (map (a: { name = "content_addressable/sha256/${a.sha256}/file"; path = fetchArchive a; })
      deps.archives);

  # NOTE: the module proxy protocol (`go help goproxy`) escapes each
  # upper-case letter of a module path as `!` and the lower-case letter.
  goEscape = s: lib.concatMapStrings
    (c: if lib.toLower c != c then "!" + lib.toLower c else c)
    (lib.stringToCharacters s);

  goProxy = pkgs.linkFarm "envoy-${version}-go-proxy" (lib.concatMap
    (m:
      let base = "${goEscape m.importpath}/@v/${m.version}"; in [
        { name = "${base}.zip"; path = pkgs.fetchurl { inherit (m.zip) url sha256; }; }
        { name = "${base}.mod"; path = pkgs.fetchurl { inherit (m.mod) url sha256; }; }
        { name = "${base}.info"; path = pkgs.writeText "${m.name}.info" (builtins.toJSON { Version = m.version; }); }
      ])
    deps.goModules);

  # NOTE: the Cargo.Bazel.lock digest includes the `rustc --version` line,
  # so crate_universe accepts the checked-in lock only with the exact Rust
  # release that Envoy pins. nixpkgs ships a newer one.
  rust = (fenix.packages.${system}.fromManifest {
    inherit (deps.rust) date pkg;
    profiles = { };
  }).withComponents [ "cargo" "rustc" "rust-std" ];

  cargoBazel = import ./cargo-bazel.nix {
    inherit pkgs;
    rulesRust = archive "rules_rust";
  };

  # NOTE: bazel/repo.bzl reads one LLVM prefix: bin/clang for the version,
  # lib/libclang.so and the clang resource directory. nixpkgs splits them.
  llvm = pkgs.symlinkJoin {
    name = "envoy-llvm-${pkgs.llvmPackages.llvm.version}";
    paths = [
      (lib.getBin pkgs.llvmPackages.llvm)
      (lib.getBin pkgs.llvmPackages.clang-unwrapped)
      (lib.getLib pkgs.llvmPackages.clang-unwrapped)
      (lib.getLib pkgs.llvmPackages.libclang)
    ];
  };

  bash = "${pkgs.bash}/bin/bash";

  src = pkgs.runCommand "envoy-${version}-src" { } ''
    cp -r ${upstream} $out
    chmod -R u+w $out
    cd $out
    patch -p1 < ${./patches/envoy-nix-toolchains.patch}
    echo ${rev} > SOURCE_VERSION

    mkdir -p bazel/nix
    substitute ${./bazel-nix.BUILD.bazel} bazel/nix/BUILD.bazel --subst-var-by bash ${bash}
    substitute ${./patches/cel-cpp-antlr.patch} bazel/nix/cel-cpp-antlr.patch \
      --subst-var-by bash ${bash} \
      --subst-var-by antlr4 ${pkgs.antlr4_13}/bin/antlr4
    ln -s ${rust}/bin/cargo bazel/nix/cargo
    ln -s ${rust}/bin/rustc bazel/nix/rustc
    ln -s ${rust}/bin/rustdoc bazel/nix/rustdoc
    ln -s ${rust} bazel/nix/rustcroot
    ln -s ${cargoBazel}/bin/cargo-bazel bazel/nix/cargo-bazel

    # NOTE: the sandbox has no /usr/bin/env and no /bin/bash.
    grep -rlIE '${shebangs}' . | xargs -r ${rewriteShebang}
    sed -i '/"-Werror"/d' bazel/envoy_internal.bzl
  '';

  mkDepsCheck = depsFile: pkgs.runCommand "envoy-${version}-deps-check"
    { nativeBuildInputs = [ python ]; } ''
    python3 ${./generate-deps.py} ${upstream} \
      --repository-cache ${repositoryCache} --previous ${depsFile} -o deps.json
    if ! diff -u ${depsFile} deps.json; then
      echo "envoy deps.json is stale: run generate-deps.py ENVOY_SRC --previous deps.json -o deps.json" >&2
      exit 1
    fi
    touch $out
  '';

  shebangs = "#! *(/usr/bin/env|(/usr)?/bin/bash)";
  rewriteShebang = pkgs.writeShellScript "envoy-rewrite-shebang" ''
    sed -i -E \
      -e 's|#! */usr/bin/env +bash\b|#!${bash}|g' \
      -e 's|#! */usr/bin/env +sh\b|#!${bash}|g' \
      -e 's|#! */usr/bin/env +python3?\b|#!${python}/bin/python3|g' \
      -e 's|#! *(/usr)?/bin/bash\b|#!${bash}|g' \
      "$@"
  '';

  bazelFlags = [
    "--repository_cache=${repositoryCache}"
    "--repository_disable_download"
    "--repo_env=BAZEL_HTTP_RULES_URLS_AS_DEFAULT_CANONICAL_ID=0"
    "--repo_env=GOPROXY=file://${goProxy}"
    "--repo_env=GOSUMDB=off"
    "--repo_env=GOTOOLCHAIN=local"
    # NOTE: the Rust dynamic module SDK runs bindgen on libclang, and the
    # builtin modules rename their symbols with llvm-objcopy, both from this
    # path. Unset, Envoy downloads a prebuilt LLVM.
    "--repo_env=BAZEL_LLVM_PATH=${llvm}"
  ];

  buildFlags = [
    "-c opt"
    "--spawn_strategy=standalone"
    "--verbose_failures"
    "--define=wasm=disabled"
    # NOTE: Bazel's strict action PATH holds only a few nixpkgs utilities.
    # Actions need the build PATH for bash, the compiler, Python and Go.
    "--action_env=PATH"
    "--host_action_env=PATH"
    # NOTE: CMake 4 refuses the `cmake_minimum_required` of the pinned libevent.
    "--action_env=CMAKE_POLICY_VERSION_MINIMUM=3.5"
    "--extra_toolchains=//bazel/nix:rust_nix_x86_64,//bazel/nix:rust_nix_aarch64,//bazel/nix:local_sh"

    # NOTE: the `gcc` config of .bazelrc, without its RBE host platform and
    # its gold flags. That platform names a remote toolchain.
    "--config=libstdc++"
    "--action_env=BAZEL_COMPILER=gcc"
    "--action_env=CC=gcc"
    "--action_env=CXX=g++"
    "--action_env=BAZEL_LINKOPTS=-lm"
    "--copt=-fno-debug-types-section"
    "--copt=-Wno-error=restrict"
    "--copt=-Wno-error=uninitialized"
    "--cxxopt=-Wno-missing-requires"
    "--cxxopt=-Wno-dangling-reference"
    "--cxxopt=-Wno-nonnull-compare"
    "--cxxopt=-Wno-trigraphs"
    "--linkopt=-Wl,-z,noexecstack"
  ];
in
pkgs.stdenv.mkDerivation {
  pname = "envoy";
  inherit version src;

  nativeBuildInputs = [
    bazel
    python
    pkgs.go
    pkgs.cmake
    pkgs.ninja
    pkgs.gnumake
    pkgs.perl
    pkgs.which
    pkgs.git
  ];
  buildInputs = [ pkgs.linuxHeaders ];

  dontUseCmakeConfigure = true;
  dontUseNinjaBuild = true;
  dontUseNinjaInstall = true;
  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    export HOME=$NIX_BUILD_TOP/home
    mkdir -p $HOME
    bazelBuild() {
      bazel --batch --output_user_root=$NIX_BUILD_TOP/bazel \
        build ${lib.concatStringsSep " " (bazelFlags ++ buildFlags)} \
        --jobs=$NIX_BUILD_CORES "$@" //source/exe:envoy-static
    }

    # NOTE: the sandbox has no /usr/bin/env and no /bin/bash. The fetched
    # repositories hold scripts that start with them, and rules
    # (rules_python, rules_foreign_cc) that write them into generated
    # scripts. The build fetches first and rewrites them. A rewritten .bzl
    # file changes the key of the repository rules it defines, so Bazel
    # extracts those archives again; the second pass rewrites them too.
    shebangs='${shebangs}'
    for pass in 1 2; do
      bazelBuild --nobuild
      grep -rlIE "$shebangs" "$NIX_BUILD_TOP"/bazel/*/external/ \
        | xargs -r ${rewriteShebang}
    done
    bazelBuild
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm0755 bazel-bin/source/exe/envoy-static $out/bin/envoy
    runHook postInstall
  '';

  requiredSystemFeatures = [ "big-parallel" ];

  passthru = {
    inherit bazel cargoBazel goProxy llvm repositoryCache rust upstream bazelFlags buildFlags mkDepsCheck;
    depsCheck = mkDepsCheck ./deps.json;
  };

  meta = {
    homepage = "https://envoyproxy.io";
    description = "Cloud-native edge and service proxy";
    license = lib.licenses.asl20;
    mainProgram = "envoy";
    platforms = [ "x86_64-linux" "aarch64-linux" ];
  };
}
