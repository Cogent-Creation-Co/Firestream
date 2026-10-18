{ pkgs, fenix, crane, system
# Optional Python packaging inputs (for mkPythonWorkspaceContainer)
, pyproject-nix ? null
, uv2nix ? null
, pyproject-build-systems ? null
}:

let
  lib = pkgs.lib;

  # Import Rust module (Fenix + Crane for incremental builds)
  rustModule = import ./rust { inherit pkgs fenix crane system; };

  # Import custom packages
  packages = import ./packages {
    inherit pkgs lib;
    mkRustPackage = rustModule.mkRustPackage;
  };
  waitForPortPkg = packages.wait-for-port;
  firestreamVibPkg = packages.firestream-vib;
  firestreamHealthdPkg = packages.firestream-healthd;

  # Import core library modules with dependency injection
  # Each module receives only the dependencies it needs
  logModule = import ./lib/log.nix { inherit pkgs lib; };
  validationsModule = import ./lib/validations.nix { inherit pkgs lib logModule; };
  fsModule = import ./lib/fs.nix { inherit pkgs lib logModule; };
  osModule = import ./lib/os.nix { inherit pkgs lib logModule fsModule validationsModule; };
  netModule = import ./lib/net.nix { inherit pkgs lib logModule validationsModule waitForPortPkg; };
  serviceModule = import ./lib/service.nix { inherit pkgs lib logModule validationsModule; };
  fileModule = import ./lib/file.nix { inherit pkgs lib logModule; };
  persistenceModule = import ./lib/persistence.nix { inherit pkgs lib logModule fsModule; };
  configModule = import ./lib/config.nix { inherit pkgs lib logModule; };
  stateModule = import ./lib/state.nix { inherit pkgs lib logModule fsModule configModule; };
  volumesModule = import ./lib/volumes.nix { inherit pkgs lib logModule fsModule; };

  # Metadata module (separate from coreLibs as it generates derivations, not shell functions)
  metadataModule = import ./lib/metadata.nix { inherit pkgs lib firestreamVibPkg; };

  # Manifest module for fleet SBOM aggregation (no IFD)
  manifestModule = import ./lib/manifest.nix { inherit pkgs lib firestreamVibPkg; };

  # Aggregate core libraries into a single attribute set
  # These modules provide shell functions (via 'functions' attr) and runtime deps
  coreLibs = {
    log = logModule;
    validations = validationsModule;
    fs = fsModule;
    os = osModule;
    net = netModule;
    service = serviceModule;
    file = fileModule;
    persistence = persistenceModule;
    config = configModule;
    state = stateModule;
    volumes = volumesModule;
  };

  # Import environment modules
  envDefaults = import ./env/defaults.nix { inherit pkgs lib; };
  envFileLoader = import ./env/file-loader.nix { inherit pkgs lib; };

  envModules = {
    mkEnvDefaults = envDefaults.mkEnvDefaults;
    mkFileLoader = envFileLoader.mkFileLoader;
  };

  # Import application factory
  appBase = import ./apps/base.nix { inherit pkgs lib coreLibs envModules; };

  # Import container factories
  containerBase = import ./containers/base.nix {
    inherit pkgs lib coreLibs waitForPortPkg firestreamVibPkg firestreamHealthdPkg;
    mkAppModule = appBase.mkAppModule;
  };
  containerPython = import ./containers/python.nix {
    inherit pkgs lib;
    mkContainerModule = containerBase.mkContainerModule;
  };
  containerJava = import ./containers/java.nix {
    inherit pkgs lib;
    mkContainerModule = containerBase.mkContainerModule;
  };
  containerNode = import ./containers/node.nix {
    inherit pkgs lib;
    mkContainerModule = containerBase.mkContainerModule;
  };

  # Import Python workspace factory (only if Python inputs provided)
  # This provides mkPythonWorkspaceContainer which combines uv2nix with mkPythonContainerModule
  hasPythonInputs = uv2nix != null && pyproject-nix != null && pyproject-build-systems != null;

  # Self-reference for passing to python-workspace.nix
  # This is a recursive definition that works because Nix is lazy
  firestreamLib = rec {
    inherit packages waitForPortPkg firestreamHealthdPkg pythonWorkspaceLib;
    mkPythonContainerModule = containerPython.mkPythonContainerModule;
  };

  # Pure helpers behind the `pythonWorkspace` consumer seam (lock diff,
  # overrides composition). No Python packaging inputs needed, so the test
  # suite can exercise them.
  pythonWorkspaceLib = import ./containers/python-workspace-lib.nix { inherit lib; };

  containerPythonWorkspace =
    if hasPythonInputs
    then import ./containers/python-workspace.nix {
      inherit pkgs lib uv2nix pyproject-nix pyproject-build-systems firestreamLib;
      mkPythonContainerModule = containerPython.mkPythonContainerModule;
    }
    else null;

  containerModules = {
    mkContainerModule = containerBase.mkContainerModule;
    mkPythonContainerModule = containerPython.mkPythonContainerModule;
    mkJavaContainerModule = containerJava.mkJavaContainerModule;
    mkNodeContainerModule = containerNode.mkNodeContainerModule;
  };

  # Import Node.js module (development environment)
  nodeModule = import ./modules/node.nix { inherit pkgs lib; };

  # Import Node.js package builder
  nodeBuilder = import ./node { inherit pkgs lib; };

in {
  # Rust module (Fenix + Crane)
  # Usage: firestream.rust.mkRustPackage { ... }
  # Usage: firestream.rust.toolchain
  rust = rustModule;

  # Convenience: direct access to mkRustPackage
  mkRustPackage = rustModule.mkRustPackage;

  # Custom packages built from source
  # Usage: firestream.packages.wait-for-port
  inherit packages;
  inherit waitForPortPkg;
  inherit firestreamHealthdPkg;

  # Core library modules (for direct access)
  # Usage: firestream.lib.log.functions
  lib = coreLibs;

  # Metadata generation module (generates derivations, not shell functions)
  # Usage: firestream.metadata.mkContainerMetadata { ... }
  # Usage: firestream.metadata.mkMetadataForContainer containerModule { ... }
  metadata = metadataModule;

  # Fleet manifest module for SBOM aggregation (no IFD)
  # Usage: firestream.manifest.collectArtifacts { ... }
  # Usage: firestream.manifest.mkFleetManifest { artifacts = ...; version = "1.0.0"; }
  manifest = manifestModule;

  # Environment generation functions
  # Usage: firestream.env.mkEnvDefaults { ... }
  env = envModules;

  # Application module factory
  # Usage: firestream.mkAppModule { name = "kafka"; ... }
  mkAppModule = appBase.mkAppModule;

  # Container module factories
  # Usage: firestream.containers.mkContainerModule { ... }
  # Usage: firestream.containers.mkPythonContainerModule { ... }
  containers = containerModules;

  # Convenience: direct access to container factories
  mkContainerModule = containerModules.mkContainerModule;
  mkPythonContainerModule = containerModules.mkPythonContainerModule;
  mkJavaContainerModule = containerModules.mkJavaContainerModule;
  mkNodeContainerModule = containerModules.mkNodeContainerModule;

  # Python workspace container factory (requires Python packaging inputs)
  # Usage: firestream.mkPythonWorkspaceContainer { workspacePath = ./airflow; name = "airflow"; ... }
  mkPythonWorkspaceContainer =
    if containerPythonWorkspace != null
    then containerPythonWorkspace.mkPythonWorkspaceContainer
    else throw "mkPythonWorkspaceContainer requires pyproject-nix, uv2nix, and pyproject-build-systems inputs";

  # Lock-compatibility guard and overrides composition used by eval-container's
  # `pythonWorkspace` option. Usage: firestream.pythonWorkspaceLib.assertCompatibleLocks { ... }
  inherit pythonWorkspaceLib;

  # Node.js module (development environment)
  # Usage: firestream.node.packages, firestream.node.shellHook
  node = nodeModule;

  # Node.js package builder
  # Usage: firestream.mkNodePackage { pname = "my-app"; src = ./.; ... }
  mkNodePackage = nodeBuilder.mkNodePackage;

  # Convenience: combined functions from all core libs
  # Returns a single string containing all library functions
  allFunctions = lib.concatMapStringsSep "\n" (m: m.functions) (lib.attrValues coreLibs);

  # Convenience: all runtime deps from core libs
  # Returns a deduplicated list of all runtime dependencies
  allRuntimeDeps = lib.unique (lib.concatMap (m: m.runtimeDeps) (lib.attrValues coreLibs));

  # Convenience: generate a combined library script
  # Creates a single bash script file with all library functions
  combinedLibScript = pkgs.writeTextDir "opt/firestream/scripts/libfirestream.sh" ''
    #!/bin/bash
    # Combined Firestream library functions
    # Copyright Firestream. MIT License.
    # Generated by Nix - do not edit directly.

    ${lib.concatMapStringsSep "\n\n" (m: m.functions) (lib.attrValues coreLibs)}
  '';

  # Meta information about the module system
  meta = {
    name = "firestream-nix-modules";
    version = "1.0.0";
    description = "Firestream Nix Shell Module System for container initialization";
    moduleCount = builtins.length (lib.attrNames coreLibs);
    modules = lib.attrNames coreLibs;
    containerFactories = [ "mkContainerModule" "mkPythonContainerModule" "mkPythonWorkspaceContainer" "mkJavaContainerModule" "mkNodeContainerModule" ];
    hasPythonWorkspaceSupport = hasPythonInputs;
    packageBuilders = [ "mkRustPackage" "mkNodePackage" ];
    devModules = [ "node" ];
    fleetManifest = [ "collectArtifacts" "mkFleetManifest" ];
  };
}
