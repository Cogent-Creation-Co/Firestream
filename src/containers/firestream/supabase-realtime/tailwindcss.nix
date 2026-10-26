# Tailwind CSS v3 CLI, built from source for the Realtime assets
# Copyright Firestream. MIT License.
#
# The Realtime `assets.deploy` alias runs the `tailwind` Mix task, which
# needs a tailwindcss v3 executable. nixpkgs `tailwindcss_3` is the
# upstream standalone release binary, so this builds the same CLI from the
# tailwindlabs/tailwindcss source with pnpm2nix instead.
#
# ./pnpm-lock.yaml is `pnpm import` (pnpm 10.26.1, auto-install-peers off)
# of the upstream package-lock.json at the same commit, after `pluginDeps`
# below is merged into package.json `dependencies`. `pluginDeps` holds the
# plugins that the standalone binary bundles (standalone-cli/package.json),
# at the versions of standalone-cli/package-lock.json; the Realtime config
# requires two of them. To bump: change `rev` and `hash`, repeat that merge
# and `pnpm import` in the new source tree, copy the lockfile here, and run
# bin/nix/firestream/node/pnpm-lock-json.sh.
#
# The upstream build runs @swc/core and the npm esbuild binary. This build
# runs nixpkgs esbuild for the same two steps (transpile src/ to CommonJS
# lib/, bundle peers/), so no npm binary package is fetched or run.
#
# Output: $out/bin/tailwindcss and $out/lib/node_modules/tailwindcss.

{ pkgs
, lib
, pnpm2nix
}:

let
  nodejs = pkgs.nodejs_22;
  esbuild = "${pkgs.esbuild}/bin/esbuild";

  pluginDeps = {
    "@tailwindcss/aspect-ratio" = "0.4.2";
    "@tailwindcss/container-queries" = "0.1.1";
    "@tailwindcss/forms" = "0.5.9";
    "@tailwindcss/typography" = "0.5.15";
  };

  transpile = ''
    find src -type f \( -name '*.js' -o -name '*.ts' \) ! -name '*.d.ts' -print0 |
      xargs -0 ${esbuild} --outdir=lib --outbase=src --format=cjs \
        --platform=node --target=node18 --log-level=warning
  '';
in
pnpm2nix.mkPnpmPackage {
  pname = "tailwindcss";
  version = "3.4.17";
  # NOTE: the commit of tag v3.4.17.
  src = pkgs.fetchFromGitHub {
    owner = "tailwindlabs";
    repo = "tailwindcss";
    rev = "4f9f603e12b51cc53b8a09c7739b8f88c8eb87eb";
    hash = "sha256-8vzxglDhwDK7opjnCwxDiCv4v14dkI6/lD/prhWGyQ4=";
  };
  inherit nodejs;
  pnpm = pkgs.pnpm_10;
  pnpmLockJson = ./pnpm-lock.json;

  # NOTE: a platform no package names. The packages with an `os` or `cpu`
  # field are the npm binaries of @swc/core, esbuild, lightningcss, turbo
  # and fsevents, and this build runs none of them.
  targetOs = "none";
  targetCpu = "none";
  targetLibc = "none";

  extraNativeBuildInputs = [ pkgs.jq pkgs.findutils ];

  postPatch = ''
    jq --argjson add ${lib.escapeShellArg (builtins.toJSON pluginDeps)} \
      '.dependencies += $add' package.json > package.json.new
    mv package.json.new package.json
    echo auto-install-peers=false >> .npmrc
  '';

  scriptFull = ''
    # NOTE: lib/util/resolveConfig.js loads this generated file, and the
    # generator loads lib/corePlugins.js; a placeholder breaks the cycle.
    echo 'export default []' > src/corePluginList.js
    ${transpile}
    node -e '
      const { corePlugins } = require("./lib/corePlugins")
      require("fs").writeFileSync("src/corePluginList.js",
        "export default " + JSON.stringify(Object.keys(corePlugins)))
    '
    ${transpile}
    (cd src && find . -type f ! -name '*.js' ! -name '*.ts' -exec install -Dm644 {} ../lib/{} \;)

    ${esbuild} lib/cli-peer-dependencies.js --bundle --platform=node \
      --outfile=peers/index.js --define:process.env.CSS_TRANSFORMER_WASM=false \
      --log-level=warning
  '';

  postBuild = ''
    rm -rf node_modules
    pnpm install --prod --frozen-lockfile --ignore-scripts --force --offline
    rm -f node_modules/.pnpm/lock.yaml
  '';

  installPhase = ''
    runHook preInstall
    pkg=$out/lib/node_modules/tailwindcss
    mkdir -p $pkg $out/bin
    cp -a lib peers stubs nesting node_modules package.json ./*.js ./*.css $pkg/
    # NOTE: NODE_PATH lets a project config require `tailwindcss/*` and the
    # bundled plugins without an install of its own, as the standalone
    # binary allows.
    cat > $out/bin/tailwindcss <<EOF
    #!${pkgs.runtimeShell}
    export NODE_PATH=$pkg/node_modules:$out/lib/node_modules\''${NODE_PATH:+:\$NODE_PATH}
    exec ${nodejs}/bin/node $pkg/lib/cli.js "\$@"
    EOF
    chmod +x $out/bin/tailwindcss
    runHook postInstall
  '';

  meta = {
    description = "Tailwind CSS v3 CLI";
    homepage = "https://github.com/tailwindlabs/tailwindcss";
    license = lib.licenses.mit;
    mainProgram = "tailwindcss";
  };
}
