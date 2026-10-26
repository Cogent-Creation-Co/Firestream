# Supabase Studio application package
# Copyright Firestream. MIT License.
#
# apps/studio of the supabase/supabase pnpm monorepo, built with pnpm2nix as
# a Next.js standalone server. The rev is the commit of the upstream image
# tag `2026.10.12-sha-3699278`, which the self-hosted v0.8.3 compose file
# runs. ./pnpm-lock.yaml is the upstream lockfile at that rev, unchanged. To
# bump: change `rev` and `hash`, copy the new upstream pnpm-lock.yaml here,
# and run bin/nix/firestream/node/pnpm-lock-json.sh.
#
# Output: $out/lib/studio (the standalone tree; the server is
# apps/studio/server.js) and $out/bin/supabase-studio.

{ pkgs
, lib
, pnpm2nix
, fenix
}:

let
  nodejs = pkgs.nodejs_22;

  # apps/studio and the workspace packages it links to, directly or through
  # another workspace package (the `link:` closure of the lockfile importers).
  components = [
    "apps/studio"
    "packages/ai-commands"
    "packages/api-types"
    "packages/build-icons"
    "packages/common"
    "packages/config"
    "packages/dev-tools"
    "packages/eslint-config-supabase"
    "packages/icons"
    "packages/pg-meta"
    "packages/shared-data"
    "packages/tsconfig"
    "packages/ui"
    "packages/ui-patterns"
  ];

  # NOTE: sparse, because the full monorepo is about 800 MB; cone mode keeps
  # the root files (package.json, pnpm-lock.yaml, pnpm-workspace.yaml).
  src = pkgs.fetchFromGitHub {
    owner = "supabase";
    repo = "supabase";
    rev = "369927892fde63d07b199d4e80fe05ae780573d5";
    sparseCheckout = components ++ [ "patches" ];
    hash = "sha256-3gZTykiXFcU/oSiG07UvYhHRV0jP4YnlQglJYtIZU4Q=";
  };

  # NOTE: pnpm 11, because the workspace pins `packageManager: pnpm@11.13.1`
  # with `engineStrict`, and nixpkgs 25.11 has pnpm 10 only.
  # WARNING: keep path concatenation. "${pkgs.path}/…" copies all of nixpkgs
  # into the store at evaluation time; after a garbage collection the
  # evaluation fails with "path … is not valid".
  pnpm = (pkgs.callPackage (pkgs.path + "/pkgs/development/tools/pnpm/generic.nix") {
    version = "11.13.1";
    hash = "sha512-svx2g7imUlQU59E+G6KMqt3elr9m7FQL+ut+cCuB8+C+TR8pXt9/n+A5Z0Co3ORQnFgt33mJH0VD/qMtN2RfJQ==";
    inherit nodejs;
  }).overrideAttrs (_: {
    # The reflink addon and the vendored binaries are prebuilt, and the
    # offline install does not use them.
    preConfigure = "rm -rf dist/reflink.*node dist/vendor";
    # pnpm 11 ships bin/pnpm.mjs; the 10.x install phase expects pnpm.cjs.
    installPhase = ''
      runHook preInstall
      install -d $out/{bin,libexec}
      cp -R . $out/libexec/pnpm
      ln -s $out/libexec/pnpm/bin/pnpm.mjs $out/bin/pnpm
      ln -s $out/libexec/pnpm/bin/pnpx.mjs $out/bin/pnpx
      runHook postInstall
    '';
    postInstall = "";
  });

  native = ../../../../bin/nix/firestream/node/native;

  # NOTE: a strace and dlopen trace of `next build` shows that it loads
  # these three native modules and runs no other native file from
  # node_modules, so only these are built from source.
  nextSwc = import (native + "/next-swc.nix") { inherit pkgs fenix; };
  tailwindcssOxide = import (native + "/tailwindcss-oxide.nix") { inherit pkgs fenix; };
  lightningcss = import (native + "/lightningcss.nix") { inherit pkgs fenix; };
  libpgQueryWasm = import (native + "/libpg-query.nix") { inherit pkgs; };

  # NOTE: every package that the lockfile limits to an `os` or a `cpu` is a
  # platform package of prebuilt binaries (@next/swc-*, @img/sharp-*,
  # @sentry/cli-*, @supabase/cli-*, @ngrok/ngrok-*,
  # @typescript/typescript-*, ...). None of them reaches the pnpm store.
  platformPackages = lib.unique (map
    (key: builtins.head (builtins.match "(@?[^@]+)@.*" key))
    (builtins.attrNames (lib.filterAttrs (_: p: p ? os || p ? cpu)
      (builtins.fromJSON (builtins.readFile ./pnpm-lock.json)).packages)));

  # NOTE: npm packages that hold only prebuilt WebAssembly files and that no
  # component of this build installs: the lockfile has them for apps/www,
  # apps/design-system and the nitro and astro dev servers.
  wasmPackages = [
    "@electric-sql/pglite"
    "@parcel/watcher-wasm"
    "@resvg/resvg-wasm"
    "@vercel/og"
    "markdown-wasm"
    "unwasm"
    "xxhash-wasm"
    "yoga-wasm-web"
  ];

  # NOTE: the prebuilt WebAssembly files that the install puts in
  # node_modules/.pnpm. A strace of `next build` (openat) and a hook on every
  # WebAssembly compile, instantiate and Module call show that the build
  # compiles none of them. The build opens the @mswjs llhttp.wasm, but no
  # output holds it; every llhttp compile is the copy that Node.js and the
  # Next undici embed as base64. The libpg-query 17.6.0 file is replaced by
  # the source build below.
  unusedWasm = [
    "libpg-query@15.2.0_*/node_modules/libpg-query/wasm/libpg-query.wasm"
    "libpg-query@17.6.0/node_modules/libpg-query/wasm/libpg-query.wasm"
    "next@16.3.6_*/node_modules/next/dist/compiled/@mswjs/interceptors/ClientRequest/llhttp/llhttp.wasm"
    "next@16.3.6_*/node_modules/next/dist/compiled/@vercel/og/resvg.wasm"
    "next@16.3.6_*/node_modules/next/dist/compiled/@vercel/og/yoga.wasm"
    "next@16.3.6_*/node_modules/next/dist/compiled/source-map08/mappings.wasm"
    "nitro@3.0.260903-beta/node_modules/nitro/dist/node_modules/source-map/lib/mappings.wasm"
    "shiki@3.13.0/node_modules/shiki/dist/onig.wasm"
    "source-map@0.7.6/node_modules/source-map/lib/mappings.wasm"
  ];

  pnpmPath = name: "node_modules/.pnpm/${name}/node_modules";

  # NOTE: minimumReleaseAge makes pnpm 11 ask the registry for publish dates
  # even in a frozen, offline install; the lockfile pins every version.
  workspaceYaml = pkgs.runCommand "pnpm-workspace.yaml" { } ''
    sed 's/^minimumReleaseAge: .*/minimumReleaseAge: 0/' ${src}/pnpm-workspace.yaml > $out
  '';
in
pnpm2nix.mkPnpmPackage {
  pname = "supabase-studio";
  version = "2026.10.12";
  workspace = src;
  inherit components nodejs pnpm;
  pnpmWorkspaceYaml = workspaceYaml;
  pnpmLockJson = ./pnpm-lock.json;
  extraNodeModuleSources = [ { name = "patches"; value = src + "/patches"; } ];
  excludePackages = platformPackages ++ wasmPackages;

  extraNativeBuildInputs = [ pkgs.findutils ];

  # WARNING: the build installs from ./pnpm-lock.json, not from the lockfile
  # in `src`. Without this test, a new `rev` builds against the old lock.
  preConfigure = ''
    if ! cmp -s pnpm-lock.yaml ${./pnpm-lock.yaml}; then
      echo "pnpm-lock.yaml differs from the upstream lockfile at rev ${src.rev}" >&2
      exit 1
    fi
  '';

  postPatch = ''
    # The sandbox has no network for next/font/google.
    cp "${pkgs.manrope}/share/fonts/truetype/Manrope[wght].ttf" apps/studio/fonts/Manrope-Variable.ttf
    substituteInPlace apps/studio/fonts/index.ts \
      --replace-fail "import { Manrope } from 'next/font/google'" "" \
      --replace-fail "export const manrope = Manrope({" \
        "export const manrope = localFont({ src: './Manrope-Variable.ttf', weight: '200 800',"

    # These point the docs search at a hosted Supabase project; a self-host
    # build keeps no link to Supabase-hosted services.
    sed -i '/^NEXT_PUBLIC_SUPABASE_URL=/d; /^NEXT_PUBLIC_SUPABASE_ANON_KEY=/d' apps/studio/.env
  '';

  buildEnv = {
    NEXT_TELEMETRY_DISABLED = "1";
    NODE_OPTIONS = "--max-old-space-size=8192";
    STUDIO_FRAMEWORK = "next";
  };

  # WARNING: the machine-code test proves that the build runs no prebuilt
  # native or WebAssembly file. Do not drop it, and do not exempt a path from
  # it: build the file from source and install it below the test. A file in
  # `unusedWasm` that is gone after a bump stops the build, so the list
  # cannot go stale.
  preBuild = ''
    for f in ${lib.concatMapStringsSep " " (f: "node_modules/.pnpm/${f}") unusedWasm}; do
      if [ ! -f "$f" ]; then
        echo "unusedWasm names no file: $f" >&2
        exit 1
      fi
      rm "$f"
    done
    node ${native + "/find-machine-code.cjs"} node_modules
    node ${native + "/find-machine-code.cjs"} --native-only "$(pnpm store path)"

    # NOTE: a package, not NEXT_TEST_NATIVE_DIR: the loader of next requires
    # the platform package and checks its version, and without it falls back
    # to a registry download of @next/swc-wasm-nodejs.
    for modules in node_modules/.pnpm/next@16.3.6*/node_modules; do
      swc="$modules"/@next/swc-linux-x64-gnu
      install -Dm555 ${nextSwc}/next-swc.linux-x64-gnu.node "$swc"/next-swc.linux-x64-gnu.node
      echo '{"name":"@next/swc-linux-x64-gnu","version":"${nextSwc.version}","main":"next-swc.linux-x64-gnu.node"}' > "$swc"/package.json
    done

    # NOTE: the napi-rs loaders of these two take a file in their own
    # package before the excluded platform package.
    install -m555 ${tailwindcssOxide}/tailwindcss-oxide.linux-x64-gnu.node \
      ${pnpmPath "@tailwindcss+oxide@4.2.4"}/@tailwindcss/oxide/
    install -m555 ${lightningcss}/lightningcss.linux-x64-gnu.node \
      ${pnpmPath "lightningcss@1.32.0"}/lightningcss/

    # NOTE: before the build, because the output file trace of `next build`
    # copies these files into the standalone tree.
    install -m444 ${libpgQueryWasm}/libpg-query.js ${libpgQueryWasm}/libpg-query.wasm \
      ${pnpmPath "libpg-query@17.6.0"}/libpg-query/wasm/
  '';

  scriptFull = "pnpm --filter studio exec next build";

  # WARNING: every binary in the standalone tree ships in the image. The
  # check `supabase-no-prebuilt-binaries` fails on any binary that no
  # allowlist entry in nix/flake-modules/supabase-pinning.nix ties to a
  # source build.
  installPhase = ''
    runHook preInstall
    mkdir -p $out/lib/studio $out/bin
    cp -a apps/studio/.next/standalone/. $out/lib/studio/
    mkdir -p $out/lib/studio/apps/studio/.next
    cp -a apps/studio/.next/static $out/lib/studio/apps/studio/.next/static
    cp -a apps/studio/public $out/lib/studio/apps/studio/public
    # pnpm links to packages that the Next.js trace left out, such as sharp.
    find $out/lib/studio -xtype l -delete

    cat > $out/bin/supabase-studio <<EOF
    #!${pkgs.runtimeShell}
    cd $out/lib/studio
    exec ${nodejs}/bin/node apps/studio/server.js "\$@"
    EOF
    chmod +x $out/bin/supabase-studio
    runHook postInstall
  '';

  passthru = { inherit nodejs pnpm nextSwc tailwindcssOxide lightningcss libpgQueryWasm; };

  meta = {
    description = "Supabase Studio, the dashboard for a self-hosted Supabase stack";
    homepage = "https://github.com/supabase/supabase/tree/master/apps/studio";
    license = lib.licenses.asl20;
    mainProgram = "supabase-studio";
  };
}
