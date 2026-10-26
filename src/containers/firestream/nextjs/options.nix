# Next.js Container Options
# Copyright Firestream. MIT License.
#
# Declarative configuration for the Next.js container, consumed by
# bin/nix/firestream/containers/eval-container.nix (runtimeType = "system").
# The module.nix factory builds the app with mkNodePackage (Next.js standalone
# output) and wraps it with mkNodeContainerModule.
#
# NET-NEW supported app: unlike the Bitnami-forked apps, the application source
# itself is Firestream's. `vendoredApp` is the override seam (mirrors odoo's
# `vendoredAddons`) — a downstream example supplies its OWN Next.js source +
# npmDepsHash, and it is forwarded to module.nix through the eval-container
# `extraModuleArgs` splice.
#
# IMPORTANT: env defaults use a PER-LEAF mkDefault so a consumer overriding one
# key does not silently drop its siblings.

{ lib, config, ... }:

let
  inherit (lib) mkOption types;

  # One vendored Next.js application source. `src` is a path/derivation holding
  # a Next.js project (package.json + committed package-lock.json +
  # next.config.js with output:"standalone"). `npmDepsHash` is the
  # buildNpmPackage fixed-output hash for that exact lockfile. When `src` is
  # null, module.nix falls back to the canonical ./app baked into this repo.
  vendoredAppType = types.submodule {
    options = {
      src = mkOption {
        type = types.nullOr (types.either types.path types.package);
        default = null;
        description = ''
          A Next.js project tree (path or derivation). When set, it REPLACES the
          canonical Firestream app baked at ./app. Must contain package.json, a
          committed package-lock.json, and a next.config.js using
          `output: "standalone"`.
        '';
      };
      npmDepsHash = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          buildNpmPackage `npmDepsHash` for the vendored app's lockfile. Obtain
          it by building once with lib.fakeHash and copying the reported
          `got: sha256-...`. Required for an npm app (committed package-lock.json).
        '';
      };

      # --- pnpm workspace (monorepo) seam ---------------------------------------
      # Setting pnpmDepsHash switches module.nix to the pnpm builder. Then `src`
      # is the WORKSPACE ROOT (holds pnpm-workspace.yaml + pnpm-lock.yaml), and
      # the target Next.js app lives at `appDir` inside it. next.config must set
      # `output:"standalone"` AND `outputFileTracingRoot` = workspace root, so the
      # standalone server lands at .next/standalone/<appDir>/server.js.
      pnpmDepsHash = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          `pkgs.pnpm.fetchDeps` FOD hash for the workspace lockfile. Obtain it by
          building once (fakeHash) and copying the reported `got: sha256-...`.
          When set, the app is built with pnpm (workspace-aware) instead of npm.
        '';
      };
      appDir = mkOption {
        type = types.str;
        default = ".";
        description = ''
          Path of the target Next.js package relative to the workspace root
          (`src`), e.g. "src/app/web". Used for the pnpm `--filter` and for the
          nested standalone paths (.next/standalone/<appDir>/server.js).
        '';
      };
      packageName = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          The target app's package.json `name` for `pnpm --filter` (e.g. "web").
          Defaults to baseNameOf appDir.
        '';
      };
      buildBefore = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Workspace packages to build (their `build` script) BEFORE the app, e.g.
          Vite libraries the app imports: [ "@scope/shared" "@scope/auth" ]. These
          are also included in the pnpm install filter so their devDeps are present.
        '';
      };
    };
  };
in
{
  # Build-time application override seam. Forwarded to module.nix via the
  # eval-container.nix extraModuleArgs splice (see config.nextjs.extraModuleArgs).
  options.nextjs.vendoredApp = mkOption {
    type = vendoredAppType;
    default = { src = null; npmDepsHash = null; };
    description = ''
      Override the baked Next.js application source. Downstream examples set
      `config.nextjs.vendoredApp = { src = ../app; npmDepsHash = "sha256-..."; }`
      to ship their own app, analogous to airflow's vendoredDags.
    '';
  };

  config.nextjs = {
    version = lib.mkDefault "1.0.0";

    # Forward the vendored app to module.nix through the factory seam.
    extraModuleArgs.appSource = config.nextjs.vendoredApp;

    # Paths configuration (per-key mkDefault for independent override).
    paths = {
      base = lib.mkDefault "/opt/firestream/nextjs";
      conf = lib.mkDefault "/opt/firestream/nextjs/config";
      data = lib.mkDefault "/firestream/nextjs/data";
      logs = lib.mkDefault "/opt/firestream/nextjs/logs";
    };

    # Environment variables with defaults. PER-LEAF mkDefault (see header).
    # The PG* libpq vars are the DB connection seam: the Helm chart overwrites
    # them from the bundled firestream-postgresql subchart. DATABASE_URL, when
    # set, wins inside the app (lib/db.js).
    env = builtins.mapAttrs (_: lib.mkDefault) {
      NODE_ENV = "production";
      PORT = "3000";
      HOSTNAME = "0.0.0.0";
      NEXT_TELEMETRY_DISABLED = "1";

      # Next.js standalone writes its incremental cache here; the chart mounts a
      # writable emptyDir at this path (readOnlyRootFilesystem otherwise blocks it).
      NEXT_CACHE_DIR = "/opt/firestream/nextjs/.next/cache";

      # Database connection (libpq-style). Defaults target the bundled subchart.
      PGHOST = "postgresql";
      PGPORT = "5432";
      PGUSER = "firestream";
      PGDATABASE = "firestream_nextjs";
      PGPASSWORD = "";
      DATABASE_URL = "";
    };

    # Variables that support Docker secrets (_FILE suffix) / K8s injection.
    envSecrets = lib.mkDefault [
      "PGPASSWORD"
      "DATABASE_URL"
    ];

    exposedPorts = lib.mkDefault [ 3000 ];

    # Docker-compose topology: nextjs + a firestream-postgresql sidecar so
    # `.#nextjs-up` is a working out-of-the-box stack. Whole-block mkDefault.
    compose = lib.mkDefault {
      projectName = "firestream-nextjs";
      dependencies = [ "postgresql" ];

      sharedEnv = {
        PGHOST = "postgresql";
        PGPORT = "5432";
        PGUSER = "firestream";
        PGDATABASE = "firestream_nextjs";
        PGPASSWORD = "firestream";
      };

      volumes = {
        postgresql_data = { };
      };

      services = {
        postgresql = {
          image = "@postgresql";
          env = {
            POSTGRESQL_DATABASE = "firestream_nextjs";
            POSTGRESQL_USERNAME = "firestream";
            POSTGRESQL_PASSWORD = "firestream";
            ALLOW_EMPTY_PASSWORD = "no";
          };
          ports = [ 5432 ];
          volumes = [ "postgresql_data:/firestream/postgresql" ];
          healthcheck = {
            test = [
              "CMD"
              "bash"
              "-c"
              "exec 3<>/dev/tcp/127.0.0.1/9180 && printf 'GET /readyz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200'"
            ];
            interval = "10s";
            timeout = "5s";
            retries = 5;
            start_period = "30s";
          };
        };
        nextjs = {
          ports = [ 3000 ];
          dependsOn = [ "postgresql" ];
          healthcheck = {
            test = [
              "CMD"
              "bash"
              "-c"
              "exec 3<>/dev/tcp/127.0.0.1/3000 && printf 'GET /api/health HTTP/1.0\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200'"
            ];
            interval = "10s";
            timeout = "5s";
            retries = 5;
            start_period = "20s";
          };
        };
      };
    };
  };
}
