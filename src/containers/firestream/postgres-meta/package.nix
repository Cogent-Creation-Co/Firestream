# postgres-meta application package
# Copyright Firestream. MIT License.
#
# supabase/postgres-meta built from source with pnpm2nix. Upstream is an npm
# project; ./pnpm-lock.yaml is `pnpm import` (pnpm 10.26.1) of the upstream
# package-lock.json at the same tag, so the versions are the upstream ones.
# To bump: change `version` and `hash`, then run `pnpm import` again in the
# new source tree, copy the result here, and run
# bin/nix/firestream/node/pnpm-lock-json.sh.
#
# Output: $out/lib/postgres-meta/{dist,node_modules,package.json} and
# $out/bin/postgres-meta.

{ pkgs
, lib
, pnpm2nix
, fenix
, version
}:

let
  nodejs = pkgs.nodejs_22;

  # NOTE: the node-gyp that ships with `nodejs`, not pkgs.node-gyp: its
  # wrapper exports npm_config_nodedir for the default Node.js, which wins
  # over --nodedir, and an addon built against other headers fails dlopen.
  nodeGyp = "${nodejs}/bin/node ${nodejs}/lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js --nodedir=${nodejs}";

  # NOTE: the commit of tag `v${version}`; module.nix sets `version`.
  src = pkgs.fetchFromGitHub {
    owner = "supabase";
    repo = "postgres-meta";
    rev = "9a2effd147f2f028fa36553ec2c7eeec80d7f8fc";
    hash = "sha256-VWZKGzru01YsaO/TWMc9W4ph+QBrplX/6Rl8V4Q6oVw=";
  };

  # The napi binding behind `oxfmt`, which @supabase/postgrest-typegen calls
  # to format generated TypeScript. Built from the commit of tag
  # oxfmt_v0.66.0, which published the npm package of the same version.
  # NOTE: a pinned 1.98.0 toolchain, because the oxc workspace declares
  # rust-version 1.96 and the nixpkgs and fenix-stable compilers of our pins
  # are older.
  oxfmtBinding =
    let
      # NOTE: an eval-time fetch of the channel manifest, not
      # fenix.toolchainOf: with a sha256, toolchainOf reads the manifest from
      # a pkgs.fetchurl output, which is import-from-derivation.
      toolchain = (fenix.fromManifestFile (builtins.fetchurl {
        url = "https://static.rust-lang.org/dist/channel-rust-1.98.0.toml";
        sha256 = "sha256-P30Tm3O7vQAE725YtDCDHGjNrSsfZO4us11UwJGZSJo=";
      })).minimalToolchain;
      rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
      oxc = pkgs.fetchFromGitHub {
        owner = "oxc-project";
        repo = "oxc";
        rev = "0b4e2e67f4193e7ebfcc64982275eb583ae82c83";
        hash = "sha256-+dWIpUPV0NrDajNXOCHuL+q4RmqjPowjzIbrdI+mBTc=";
      };
    in
    rustPlatform.buildRustPackage {
      pname = "oxfmt-napi-binding";
      version = "0.66.0";
      src = oxc;
      # NOTE: cargoHash (a fetchCargoVendor vendor), not cargoLock.lockFile:
      # a Cargo.lock read from the fetched oxc source at eval time is
      # import-from-derivation.
      cargoHash = "sha256-dCZqFCSMgnU9Kyjxkoqxq2BiTwu82d1HFeFSyFavj38=";
      cargoBuildFlags = [ "-p" "oxfmt" "--lib" "--features" "allocator" ];
      nativeBuildInputs = [ pkgs.cmake ];
      dontUseCmakeConfigure = true;
      doCheck = false;
      installPhase = ''
        install -Dm555 target/*/release/liboxfmt.so $out/oxfmt.linux-x64-gnu.node
      '';
    };

  libpgQueryWasm = import ../../../../bin/nix/firestream/node/native/libpg-query.nix { inherit pkgs; };

  pnpmPath = name: "node_modules/.pnpm/${name}/node_modules";
in
pnpm2nix.mkPnpmPackage {
  pname = "postgres-meta";
  inherit version src nodejs;
  pnpm = pkgs.pnpm_10;
  pnpmLockJson = ./pnpm-lock.json;

  extraNativeBuildInputs = [ pkgs.python3 pkgs.findutils ];

  script = "build";

  # WARNING: every binary left in node_modules after this hook ships in the
  # image. The check `supabase-no-prebuilt-binaries` fails on any binary
  # that no allowlist entry in nix/flake-modules/supabase-pinning.nix ties to
  # a source build; do not add an entry to get past a new upstream
  # dependency.
  postBuild = ''
    rm -rf node_modules
    pnpm install --prod --frozen-lockfile --ignore-scripts --force --offline

    # NOTE: `pnpm import` writes no `libc` field, so the platform filter
    # keeps the musl variants of the native packages.
    rm -rf node_modules/.pnpm/*-musl@* node_modules/.pnpm/node_modules/*/*-musl

    # piscina loads @napi-rs/nice only for a non-zero niceIncrement, and
    # ignores a load failure.
    rm -rf node_modules/.pnpm/@napi-rs+nice*
    find node_modules -xtype l -delete

    profiler=${pnpmPath "@sentry-internal+node-cpu-profiler@2.1.0"}/@sentry-internal/node-cpu-profiler
    rm -f "$profiler"/lib/*.node
    (cd "$profiler" && ${nodeGyp} rebuild --jobs="$NIX_BUILD_CORES")
    # The loader takes lib/sentry_cpu_profiler-<platform>-<arch>-<libc>-<abi>.node.
    abi=$(node -p process.versions.modules)
    install -m555 "$profiler"/build/Release/sentry_cpu_profiler.node \
      "$profiler"/lib/sentry_cpu_profiler-linux-x64-glibc-$abi.node
    rm -rf "$profiler"/build

    binding=${pnpmPath "@oxfmt+binding-linux-x64-gnu@0.66.0"}/@oxfmt/binding-linux-x64-gnu
    rm -f "$binding"/*.node
    install -m555 ${oxfmtBinding}/oxfmt.linux-x64-gnu.node "$binding"/

    pgQuery=${pnpmPath "libpg-query@17.6.0"}/libpg-query
    install -m444 ${libpgQueryWasm}/libpg-query.js ${libpgQueryWasm}/libpg-query.wasm "$pgQuery"/wasm/

    # NOTE: pnpm's own copy of the patched lockfile names every fetched
    # tarball, so the image closure would hold all of them.
    rm -f node_modules/.pnpm/lock.yaml
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/lib/postgres-meta $out/bin
    cp -a dist node_modules package.json $out/lib/postgres-meta/
    cat > $out/bin/postgres-meta <<EOF
    #!${pkgs.runtimeShell}
    cd $out/lib/postgres-meta
    exec ${nodejs}/bin/node dist/server/server.js "\$@"
    EOF
    chmod +x $out/bin/postgres-meta
    runHook postInstall
  '';

  passthru = { inherit nodejs oxfmtBinding libpgQueryWasm; };

  meta = {
    description = "Supabase postgres-meta: a RESTful API for managing Postgres";
    homepage = "https://github.com/supabase/postgres-meta";
    license = lib.licenses.asl20;
    mainProgram = "postgres-meta";
  };
}
