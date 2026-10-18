{
  description = "Firestream";

  inputs = {
    # Fixed nixpkgs version fixes system packages that float on the latest Debian
    nixpkgs.url = "github:NixOS/nixpkgs/release-25.11";

    # flake-utils for container flake imports
    flake-utils.url = "github:numtide/flake-utils";

    # flake-parts for modular flake composition (Phase 3: now the top-level driver)
    flake-parts.url = "github:hercules-ci/flake-parts";

    # Python packaging infrastructure for containers
    pyproject-nix = {
      url = "github:pyproject-nix/pyproject.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    uv2nix = {
      url = "github:pyproject-nix/uv2nix";
      inputs.pyproject-nix.follows = "pyproject-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    pyproject-build-systems = {
      url = "github:pyproject-nix/build-system-pkgs";
      inputs.pyproject-nix.follows = "pyproject-nix";
      inputs.uv2nix.follows = "uv2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # gitignore.nix for faster source filtering (respects .gitignore)
    gitignore = {
      url = "github:hercules-ci/gitignore.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Fenix for deterministic Rust toolchain
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Crane for incremental Rust builds with dependency caching
    crane.url = "github:ipetkov/crane";
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } ({ config, withSystem, lib, ... }: {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      imports = [
        ./nix/flake-modules/framework.nix
        ./nix/flake-modules/rust.nix
        ./nix/flake-modules/util.nix
        ./nix/flake-modules/ci-profile.nix
        ./nix/flake-modules/devshell.nix
        ./nix/flake-modules/checks.nix
        ./nix/flake-modules/registry.nix
        ./nix/flake-modules/aggregate.nix
        ./nix/flake-modules/docker-build.nix
        ./nix/flake-modules/compose.nix
        ./nix/flake-modules/containers/airflow.nix
        ./nix/flake-modules/containers/jupyterhub.nix
        ./nix/flake-modules/containers/superset.nix
        ./nix/flake-modules/containers/odoo.nix
        ./nix/flake-modules/containers/postgresql.nix
        ./nix/flake-modules/containers/redis.nix
        ./nix/flake-modules/containers/kafka.nix
        ./nix/flake-modules/containers/spark.nix
        ./nix/flake-modules/containers/seaweedfs.nix
        ./nix/flake-modules/containers/os-shell.nix
        ./nix/flake-modules/containers/nextjs.nix
        ./nix/flake-modules/charts/airflow.nix
        ./nix/flake-modules/charts/postgresql.nix
        ./nix/flake-modules/charts/redis.nix
        ./nix/flake-modules/charts/kafka.nix
        ./nix/flake-modules/charts/spark.nix
        ./nix/flake-modules/charts/jupyterhub.nix
        ./nix/flake-modules/charts/superset.nix
        ./nix/flake-modules/charts/odoo.nix
        ./nix/flake-modules/charts/seaweedfs.nix
        ./nix/flake-modules/charts/nextjs.nix
        ./nix/flake-modules/charts/aggregate.nix
        ./nix/flake-modules/charts/checks.nix
      ];

      # System-independent and cross-system flake outputs.
      # Written so it reads the OUTER config/withSystem/lib.
      flake = {
        # Firestream module system library, per system.
        # Mirrors the legacy `lib = forAllSystems (system: ...)` block, plus the
        # new `images` consumer-override API sourced from the per-system registry.
        lib = lib.genAttrs config.systems (system:
          withSystem system ({ firestreamLib, config, ... }: {
            # Complete Firestream module system
            firestream = firestreamLib;

            # Convenience: direct access to factory functions
            mkAppModule = firestreamLib.mkAppModule;
            mkContainerModule = firestreamLib.mkContainerModule;
            mkPythonContainerModule = firestreamLib.mkPythonContainerModule;
            mkPythonWorkspaceContainer = firestreamLib.mkPythonWorkspaceContainer;

            # Core libraries for custom modules
            coreLibs = firestreamLib.lib;

            # Rust module - allows external flakes to build Rust packages
            rust = firestreamLib.rust;
            mkRustPackage = firestreamLib.mkRustPackage;
            rustToolchain =
              if firestreamLib.rust != null
              then firestreamLib.rust.toolchain
              else null;

            # NEW: consumer override API. The per-system registry of images, each
            # with { dockerImage; eval; options; } enriched with the deployment
            # surface added in this phase:
            #   imageRef  - "firestream-<name>:<tag>" (matches what the flake builds)
            #   compose   - packages.<name>-compose derivation (docker-compose.yml)
            #   buildApp  - apps.<name>-image (Docker-based Linux build, runs on Darwin)
            images = builtins.mapAttrs
              (name: entry: entry // {
                imageRef = (entry.eval (_: { })).imageRef;
                compose = config.packages.${name + "-compose"} or null;
                buildApp = config.apps.${name + "-image"} or null;
              })
              config.firestreamImages;

            # NEW (Phase 4): consumer-facing chart surface. The per-system
            # registry of chart override APIs, each with
            # { chartBundle; render; eval; options; }. Exposed bare (no
            # enrichment) mirroring `images` above.
            charts = config.firestreamChartImages;
          }));

        # Overlay for nixpkgs integration
        # Usage: nixpkgs.overlays = [ firestream.overlays.default ];
        overlays.default = final: prev: {
          firestream = {
            wait-for-port = inputs.self.packages.${prev.system}.wait-for-port;
            mkRustPackage = inputs.self.lib.${prev.system}.mkRustPackage;
            rustToolchain = inputs.self.lib.${prev.system}.rustToolchain;
          };
        };

        # Home Manager module for Firestream CLI/TUI
        # Usage: imports = [ inputs.firestream.homeManagerModules.default ];
        homeManagerModules.default = import ./bin/nix/firestream/home-manager/firestream.nix;
        homeModules.default = import ./bin/nix/firestream/home-manager/firestream.nix;

        # System-independent Firestream module system
        # For importing from other flakes.
        firestreamModules = { pkgs, system }: import ./bin/nix/firestream {
          inherit pkgs system;
          inherit (inputs) fenix crane pyproject-nix uv2nix pyproject-build-systems;
        };
      };
    });
}
