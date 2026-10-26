# Elixir release builder
# Copyright Firestream. MIT License.
#
# mkMixRelease builds an Elixir `mix release` from source with nixpkgs'
# mixRelease. `mixDeps` is a mix-deps.nix that
# bin/nix/firestream/beam/mix-deps.sh writes from the app's mix.lock: one
# fetch per package (fetchHex by the lock checksum, fetchFromGitHub at the
# lock commit). The fetched sources form the `deps/` tree of the build.
#
# Optional seams, each off by default:
#   nif    - a Rust NIF that rustler compiles during the build, from crates
#            that fetchCargoVendor vendors by `cargoHash` (no precompiled
#            NIF, no network)
#   assets - an npm `assets/` tree. `assets.nodeModules` is its installed
#            node_modules (pnpm2nix, one fetch per lockfile package);
#            esbuild comes from nixpkgs and the tailwind CLI from the
#            package that `assets.tailwind` names
#
# Usage:
#   mkMixRelease {
#     pname = "supavisor"; version = "2.9.13"; inherit src;
#     erlang = "erlang_27"; elixir = "elixir_1_18";
#     mixDeps = ./mix-deps.nix;
#     nif = { cargoRoot = "native"; cargoHash = "sha256-..."; };
#   }

{ pkgs, lib }:

{
  pname,
  version,
  src,

  # Attribute names in beam_minimal.packages.<erlang>.
  # NOTE: per app, not global, because the upstream projects pin different
  # OTP releases and a shared one breaks the older of them.
  erlang ? "erlang_28",
  elixir ? "elixir_1_19",

  # A mix-deps.nix path; null when the project has no Mix dependencies.
  mixDeps ? null,

  mixReleaseName ? pname,
  mixEnv ? "prod",

  nif ? null,
  assets ? null,

  nativeBuildInputs ? [ ],
  buildInputs ? [ ],
  postPatch ? "",
  postBuild ? "",
  ...
}@args:

let
  # NOTE: beam_minimal is erlang without wx and systemd. Only erts' epmd
  # links libsystemd, and it drags about 170 MiB of systemd into every
  # release closure; the cache already carries this set.
  beamPackages = pkgs.beam_minimal.packages.${erlang}.extend (self: _: {
    elixir = self.${elixir};
  });

  depSources = import mixDeps {
    inherit (beamPackages) fetchHex;
    inherit (pkgs) fetchFromGitHub;
  };

  # NOTE: a copy, not symlinks: mixRelease copies this tree without -L, and
  # rebar3 and make deps compile inside their own directory. Mix treats a Git
  # dep without .git/HEAD as not checked out, so each one gets the lock
  # commit there, as fetchMixDeps leaves it.
  depsTree = pkgs.runCommand "${pname}-${version}-mix-deps" { } (''
    mkdir $out
  '' + lib.concatStrings (lib.mapAttrsToList (name: dep: ''
    cp -r --no-preserve=mode ${dep} $out/${name}
  '' + lib.optionalString (dep ? rev) ''
    mkdir -p $out/${name}/.git
    echo ${dep.rev} > $out/${name}/.git/HEAD
  '') depSources));

  mixFodDeps = if mixDeps == null then null else depsTree;

  nifArgs = lib.optionalAttrs (nif != null) {
    # NOTE: a fixed-output vendor, not importCargoLock: importCargoLock reads
    # Cargo.lock from the fetched `src` at eval time, which is
    # import-from-derivation and fails `nix flake check --no-build`.
    cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
      name = "${pname}-${version}-nif";
      inherit src;
      inherit (nif) cargoRoot;
      hash = nif.cargoHash;
    };
    inherit (nif) cargoRoot;
    dontUseCmakeConfigure = true;
  };

  nifInputs = lib.optionals (nif != null) [
    pkgs.cargo
    pkgs.rustc
    pkgs.rustPlatform.cargoSetupHook
    pkgs.rustPlatform.bindgenHook
    pkgs.cmake
  ];

  assetsRoot = if assets == null then null else assets.npmRoot or "assets";

  tailwind = assets.tailwind or (throw
    "mkMixRelease ${pname}: assets.tailwind must name a tailwind CLI package built from source");

  # NOTE: a node_modules tree, not a fetchNpmDeps hash: fetchNpmDeps is one
  # fixed-output fetch of the whole npm tree, and it downloads every npm
  # binary package that the lockfile names.
  nodeModules = assets.nodeModules or (throw
    "mkMixRelease ${pname}: assets.nodeModules must name an installed node_modules tree; assets.npmDepsHash is not supported");

  assetArgs = lib.optionalAttrs (assets != null) {
    disallowedReferences = [ pkgs.esbuild tailwind nodeModules ];
  };

  assetInputs = lib.optionals (assets != null) [
    pkgs.removeReferencesTo
  ];

  # NOTE: sys.config keeps the esbuild and tailwind store paths from the
  # config above. They are never read at runtime, and they would ship both
  # tools in every image.
  assetReferenceStrip = lib.optionalString (assets != null) ''
    find "$out/releases" -name sys.config -exec remove-references-to \
      -t ${pkgs.esbuild} -t ${tailwind} {} +
  '';

  # NOTE: without these paths the esbuild and tailwind Mix tasks download
  # their own binaries, which the sandbox refuses.
  assetConfig = lib.optionalString (assets != null) ''
    ln -s ${nodeModules} ${assetsRoot}/node_modules
    cat >> config/config.exs <<'EOF'
    config :esbuild, path: "${pkgs.esbuild}/bin/esbuild"
    config :tailwind, path: "${lib.getExe tailwind}"
    EOF
  '';

  assetBuild = lib.optionalString (assets != null) ''
    mix do deps.loadpaths --no-deps-check, assets.deploy
  '';

  passArgs = builtins.removeAttrs args [
    "erlang" "elixir" "mixDeps" "nif" "assets"
    "nativeBuildInputs" "buildInputs" "postPatch" "postBuild" "postInstall"
  ];
in
assert lib.assertMsg (!(args ? mixDepsHash))
  "mkMixRelease ${pname}: mixDepsHash is not supported; pass mixDeps, a mix-deps.nix from bin/nix/firestream/beam/mix-deps.sh";
beamPackages.mixRelease (passArgs // nifArgs // assetArgs // {
  inherit mixFodDeps mixReleaseName mixEnv;
  nativeBuildInputs = nifInputs ++ assetInputs ++ nativeBuildInputs;
  inherit buildInputs;
  postPatch = assetConfig + postPatch;
  # NOTE: postBuild sits between `mix compile` and `mix release`, the one
  # point where the assets can compile against the release's own deps.
  postBuild = assetBuild + postBuild;
  postInstall = assetReferenceStrip + (args.postInstall or "");
  # NOTE: a baked cookie is the same in every image built from this source,
  # so any node that can reach epmd could join. The container sets
  # RELEASE_COOKIE at boot instead.
  removeCookie = true;
  passthru = (args.passthru or { }) // {
    inherit beamPackages;
    mixDepsFile = mixDeps;
  };
})
