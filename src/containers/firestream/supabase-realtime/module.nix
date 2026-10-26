# Supabase Realtime Container Module
# Copyright Firestream. MIT License.
#
# Realtime is an Elixir release, built from source with mkMixRelease and run
# with mkBeamContainerModule. Every boot runs the upstream run.sh steps before
# the server: the migrations, then (with SEED_SELF_HOST=true) the self-host
# tenant seed, which is idempotent.
#
# Gap: upstream also bakes `pgdelta` (a Bun binary from supabase/pg-toolbelt)
# for one dashboard page, the tenant migration view. This image does not
# have it; the realtime API does not use it.

{ pkgs
, lib
, firestream
, pnpm2nix

, version ? "2.143.3"

, paths ? {
    base = "/opt/firestream/realtime";
    conf = "/opt/firestream/realtime/config";
    data = "/firestream/realtime/data";
    logs = "/opt/firestream/realtime/logs";
  }

, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 4000 ]
, imageName ? "firestream-supabase-realtime"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  src = pkgs.fetchFromGitHub {
    owner = "supabase";
    repo = "realtime";
    rev = "d46540b3a4a79113f40301d5e5cb690f41150b9b";
    hash = "sha256-0o/p4BuTKCAx6e2Y4e1cOqRC5VdTrlv1Y5KvRrxAYtQ=";
  };

  # ./assets/pnpm-lock.yaml is `pnpm import` (pnpm 10.26.1,
  # auto-install-peers off) of the upstream assets/package-lock.json. To bump:
  # repeat the import in the new assets/ tree, copy the lockfile here, and run
  # bin/nix/firestream/node/pnpm-lock-json.sh.
  assetsNodeModules = pnpm2nix.mkPnpmPackage {
    pname = "realtime-assets-node-modules";
    inherit version;
    src = "${src}/assets";
    nodejs = pkgs.nodejs_22;
    pnpm = pkgs.pnpm_10;
    pnpmLockJson = ./assets/pnpm-lock.json;
    # NOTE: oxlint and oxfmt are the lint and format devDependencies. Their
    # npm packages only start a prebuilt binary, which the asset build never
    # runs; tinypool is a dependency of oxfmt alone.
    excludePackages = [ "oxlint" "oxfmt" "tinypool" ];
    noDevDependencies = true;
    # NOTE: a platform no package names, so the @oxlint/binding-* and
    # @oxfmt/binding-* binary packages drop out too.
    targetOs = "none";
    targetCpu = "none";
    targetLibc = "none";
    postPatch = "echo auto-install-peers=false >> .npmrc";
    scriptFull = "";
    distDir = "node_modules";
  };

  release = firestream.mkMixRelease {
    pname = "realtime";
    inherit version src;
    erlang = "erlang_28";
    elixir = "elixir_1_19";
    mixDeps = ./mix-deps.nix;
    assets = {
      nodeModules = assetsNodeModules;
      tailwind = import ./tailwindcss.nix { inherit pkgs lib pnpm2nix; };
    };
  };
in
firestream.mkBeamContainerModule {
  name = "realtime";
  inherit version paths envVars envVarsWithSecrets;
  inherit imageName imageTag exposedPorts health release;

  validateFn = ''
    for v in DB_PASSWORD DB_ENC_KEY API_JWT_SECRET SECRET_KEY_BASE; do
      [[ -n "''${!v:-}" ]] || print_validation_error "$v is required"
    done
  '';

  preStart = ''
    if [[ -n "''${RLIMIT_NOFILE:-}" ]]; then
      ulimit -Sn "$RLIMIT_NOFILE"
    fi
    "$RELEASE_BIN" eval 'Realtime.Release.migrate()'
    if [[ "''${SEED_SELF_HOST:-}" == true ]]; then
      "$RELEASE_BIN" eval 'Realtime.Release.seeds(Realtime.Repo)'
    fi
  '';

  enableStateTracking = false;

  user = { name = "realtime"; group = "realtime"; uid = 1001; gid = 1001; };
}
