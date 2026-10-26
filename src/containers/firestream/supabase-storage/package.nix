# Supabase Storage API application package
# Copyright Firestream. MIT License.
#
# supabase/storage built from source with pnpm2nix. Upstream is an npm
# project; ./pnpm-lock.yaml is `pnpm import` (pnpm 10.26.1) of the upstream
# package-lock.json at the same tag, with one edit: pg-boss points at commit
# 89b675b0, the commit that package-lock.json pins. `pnpm import` resolves
# the `github:supabase/pg-boss#feat/exactly_once` specifier again and takes
# the branch head instead. To bump: change `version` and `hash`, run
# `pnpm import` again, repeat the pg-boss edit for each git dependency, and
# run bin/nix/firestream/node/pnpm-lock-json.sh.
#
# Output: $out/lib/storage/{dist,migrations,node_modules,package.json,watt*.json}
# and $out/bin/storage-api.

{ pkgs
, lib
, pnpm2nix
, fenix
, version
}:

let
  nodejs = pkgs.nodejs_24;

  # NOTE: the node-gyp that ships with `nodejs`, not pkgs.node-gyp: its
  # wrapper exports npm_config_nodedir for the default Node.js, which wins
  # over --nodedir, and an addon built against other headers fails dlopen.
  nodeGyp = "${nodejs}/bin/node ${nodejs}/lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js --nodedir=${nodejs}";

  # NOTE: the commit of tag `v${version}`; module.nix sets `version`.
  src = pkgs.fetchFromGitHub {
    owner = "supabase";
    repo = "storage";
    rev = "f31599e188d7c9c854b1daa76a11b41c6d179462";
    hash = "sha256-bP478nXbS8w80RXObGcnXAk18LabT2LA8UyLvr7hjWc=";
  };

  # build.js runs esbuild; ESBUILD_BINARY_PATH makes the npm package use
  # this binary instead of the prebuilt @esbuild/linux-x64. The version must
  # equal the locked npm esbuild, which checks it at start.
  esbuild = pkgs.esbuild.overrideAttrs (_: rec {
    version = "0.28.1";
    src = pkgs.fetchFromGitHub {
      owner = "evanw";
      repo = "esbuild";
      rev = "bb9db84c02433fbe37b3509f53f9f3e3cc48725e";
      hash = "sha256-V+HKaWGAIs24ynFFIS9fQ0EAJJdNmlAMeL1sgDEAqWM=";
    };
    vendorHash = "sha256-+BfxCyg0KkDQpHt/wycy/8CTG6YBA/VJvJFhhzUnSiQ=";
  });

  # The npm tarball of @datadog/pprof holds only prebuilt binaries, so the
  # addon is built from the git commit that the tarball names (gitHead).
  datadogPprof = pkgs.stdenv.mkDerivation {
    pname = "datadog-pprof-addon";
    version = "5.15.1";
    src = pkgs.fetchFromGitHub {
      owner = "DataDog";
      repo = "pprof-nodejs";
      rev = "110d171c731fa17c7a1698d3acef08ae4e6a1f8d";
      hash = "sha256-9L38MizjPf6A39npJowI01bdfYuKVwWL/CvQhuLVHaI=";
    };
    # The headers that binding.gyp takes from node_modules/nan, at the
    # commit of tag v2.27.0, the version that the package-lock.json of
    # pprof-nodejs locks.
    nan = pkgs.fetchFromGitHub {
      owner = "nodejs";
      repo = "nan";
      rev = "740ef762f81dc4abab2c9572a030cf19edc97701";
      hash = "sha256-5OstellV0hSc/eQF/YO47BoApLXUj7DWgTRL3Jz60+E=";
    };
    nativeBuildInputs = [ nodejs pkgs.python3 ];
    configurePhase = ''
      mkdir -p node_modules
      cp -r --no-preserve=mode $nan node_modules/nan
      export HOME=$TMPDIR
    '';
    buildPhase = ''
      ${nodeGyp} configure build --jobs=$NIX_BUILD_CORES
    '';
    installPhase = ''
      install -Dm555 build/Release/dd_pprof.node $out/dd_pprof.node
    '';
  };

  sourceMapWasm = import ../../../../bin/nix/firestream/node/native/source-map-mappings { inherit pkgs fenix; };

  pnpmPath = name: "node_modules/.pnpm/${name}/node_modules";
in
pnpm2nix.mkPnpmPackage {
  pname = "storage";
  inherit version src nodejs;
  pnpm = pkgs.pnpm_10.override { inherit nodejs; };
  pnpmLockJson = ./pnpm-lock.json;

  extraNativeBuildInputs = [ pkgs.python3 pkgs.findutils ];

  # NOTE: hoisted, because Storage imports packages that it does not declare
  # (`@fastify/error`, `fast-uri`, `srvx`, ...); npm hoisting hides that, and
  # the strict pnpm layout fails `tsc` and the runtime require.
  installEnv = {
    npm_config_shamefully_hoist = "true";
    npm_config_package_manager_strict = "false";
    npm_config_engine_strict = "false";
  };

  buildEnv = {
    ESBUILD_BINARY_PATH = "${esbuild}/bin/esbuild";
    npm_config_package_manager_strict = "false";
  };

  # The prebuilt esbuild binaries go, so only the source build can run.
  preBuild = ''
    rm -rf node_modules/.pnpm/@esbuild+* node_modules/@esbuild
    find node_modules -xtype l -delete
  '';

  script = "build";

  # WARNING: every binary left in node_modules after this hook ships in the
  # image. The check `supabase-no-prebuilt-binaries` fails on any binary
  # that no allowlist entry in nix/flake-modules/supabase-pinning.nix ties to
  # a source build; do not add an entry to get past a new upstream
  # dependency.
  postBuild = ''
    rm -rf node_modules
    npm_config_shamefully_hoist=true npm_config_package_manager_strict=false npm_config_engine_strict=false \
      pnpm install --prod --frozen-lockfile --ignore-scripts --force --offline

    # NOTE: `pnpm import` writes no `libc` field, so the platform filter
    # keeps the musl variants of the native packages.
    rm -rf node_modules/.pnpm/*-musl@* node_modules/.pnpm/node_modules/*/*-musl
    find node_modules -xtype l -delete

    xattr=${pnpmPath "fs-xattr@0.4.0"}/fs-xattr
    (cd "$xattr" && ${nodeGyp} rebuild --jobs="$NIX_BUILD_CORES")
    mv "$xattr"/build/Release/xattr.node .
    rm -rf "$xattr"/build
    install -Dm555 xattr.node "$xattr"/build/Release/xattr.node

    # node-gyp-build loads build/Release before prebuilds/.
    pprof=${pnpmPath "@datadog+pprof@5.15.1"}/@datadog/pprof
    rm -rf "$pprof"/prebuilds
    install -Dm555 ${datadogPprof}/dd_pprof.node "$pprof"/build/Release/dd_pprof.node

    # NOTE: the bare-* packages are tar-fs dependencies for the Bare runtime
    # only: tar-fs maps them through package.json `imports` keys without a
    # `#`, which Node.js ignores, so Node.js loads neither them nor their
    # prebuilt addons.
    rm -rf node_modules/.pnpm/bare-*/node_modules/bare-*/prebuilds

    # NOTE: a source build, not a deletion: the source mapper of
    # @datadog/pprof loads this module.
    sourceMap=${pnpmPath "source-map@0.7.6"}/source-map
    install -m444 ${sourceMapWasm}/mappings.wasm "$sourceMap"/lib/mappings.wasm

    # NOTE: pnpm's own copy of the patched lockfile names every fetched
    # tarball, so the image closure would hold all of them.
    rm -f node_modules/.pnpm/lock.yaml
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/lib/storage $out/bin
    cp -a dist migrations node_modules package.json watt.json watt.storage.json $out/lib/storage/
    cat > $out/bin/storage-api <<EOF
    #!${pkgs.runtimeShell}
    cd $out/lib/storage
    exec ${nodejs}/bin/node dist/start/server.js "\$@"
    EOF
    chmod +x $out/bin/storage-api
    runHook postInstall
  '';

  passthru = { inherit nodejs esbuild datadogPprof sourceMapWasm; };

  meta = {
    description = "Supabase Storage: an S3-compatible object storage API with Postgres access rules";
    homepage = "https://github.com/supabase/storage";
    license = lib.licenses.asl20;
    mainProgram = "storage-api";
  };
}
