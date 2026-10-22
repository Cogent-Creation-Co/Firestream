# Python Container Module Factory
# Copyright Firestream. MIT License.
#
# This module provides mkPythonContainerModule - a specialized factory for
# Python applications that integrates with uv2nix/pyproject-nix for dependency
# management.
#
# Features:
# - uv2nix/pyproject-nix integration for Python dependencies
# - Automatic PYTHONPATH configuration
# - pip requirements.txt support at runtime
# - Virtual environment bundling
#
# Usage:
#   let
#     container = mkPythonContainerModule {
#       name = "airflow";
#       version = "3.0.3";
#       pythonEnv = pythonSet.mkVirtualEnv "airflow-env" deps;
#       python = pkgs.python312;
#       systemDeps = [ pkgs.postgresql ];
#       validateFn = "...";
#       runCmd = "airflow webserver";
#     };
#   in container.dockerImage

{ pkgs, lib, mkContainerModule }:

{
  # Factory function for Python container modules
  mkPythonContainerModule = {
    # Basic metadata
    name,
    version ? "1.0.0",

    # Docker image naming (parity-preserving defaults match the historical literals)
    imageName ? "firestream-${name}",
    imageTag ? version,

    # Python configuration (required)
    pythonEnv,               # The virtual environment from uv2nix/pyproject-nix
    python ? pkgs.python312, # Python interpreter

    # Environment configuration
    envVars ? {},
    envVarsWithSecrets ? [],

    # Paths configuration
    paths ? {
      base = "/opt/firestream/${name}";
      conf = "/opt/firestream/${name}/config";
      data = "/firestream/${name}/data";
      logs = "/opt/firestream/${name}/logs";
    },

    # User/group configuration
    user ? {
      name = name;
      group = name;
      uid = 1001;
      gid = 1001;
    },

    # Application-specific shell functions
    validateFn ? "",
    configFn ? "",
    initFn ? "",
    initEveryBoot ? false,
    runCmd ? "",

    # Per-container helpers emitted at top-level of libhelpers<name>.sh.
    # Forwarded to mkContainerModule (and through to mkAppModule).
    perContainerHelpers ? "",

    # Container dependencies
    systemDeps ? [],
    runtimeBinDeps ? [],
    extraDeps ? [],

    # Docker configuration
    dockerConfig ? {},
    exposedPorts ? [],
    volumes ? [],

    # In-image firestream-healthd configuration (Phase 4). Forwarded verbatim
    # to mkContainerModule, which wraps the entrypoint with a healthd launcher
    # when `enable = true`. Default-off preserves byte-identical pre-Phase-3
    # behaviour for any container that does not opt in.
    health ? { enable = false; port = 9180; readinessCmd = null; },

    # Python-specific options
    requirementsPath ? "/bitnami/python/requirements.txt",  # Runtime requirements
    enablePip ? true,        # Allow pip install at runtime
    sitePackagesPath ? null, # Override site-packages path

    # Custom scripts and hooks
    customScripts ? {},
    devShellPackages ? [],
    devShellHook ? "",

    # Build-time (prepopulate phase) - passed to mkContainerModule
    prepopulateFn ? "",
    prepopulateFiles ? {},
    prepopulateDirs ? [],
    runtimeDirs ? {},              # Declarative directory specifications (preferred)

    # Runtime (activate phase) - passed to mkContainerModule
    activateFn ? "",
    enableStateTracking ? true,

    # Python-specific build-time options
    compileByteCode ? false,        # Pre-compile .pyc files at build time
    pythonPrepopulateFn ? "",       # Python-specific build setup
  }:
  let
    # Compute site packages path
    pythonSitePackages =
      if sitePackagesPath != null
      then sitePackagesPath
      else "${pythonEnv}/${python.sitePackages}";

    # Combine base prepopulateFn with Python-specific setup
    combinedPrepopulateFn = ''
      ${prepopulateFn}

      # Python-specific prepopulation
      ${pythonPrepopulateFn}

      ${lib.optionalString compileByteCode ''
        # Pre-compile Python bytecode for faster startup
        echo "Pre-compiling Python bytecode..."
        ${python}/bin/python -m compileall -q ${pythonEnv}/lib/python*/site-packages/ 2>/dev/null || true
      ''}
    '';

    # Python-specific runtime bins
    pythonRuntimeBins = runtimeBinDeps ++ [ pythonEnv ];

    # Create wrapper for entrypoint that sets up Python environment
    pythonEntrypointWrapper = entrypoint: pkgs.runCommand "${name}-python-entrypoint" {
      nativeBuildInputs = [ pkgs.makeWrapper ];
    } ''
      mkdir -p $out/bin
      makeWrapper ${entrypoint}/bin/${name}-entrypoint $out/bin/${name}-entrypoint \
        --set PATH "${lib.makeBinPath (pythonRuntimeBins ++ systemDeps)}:/bin:/usr/bin" \
        --set PYTHONPATH "${pythonSitePackages}" \
        --set LD_LIBRARY_PATH "${lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib ]}:''${LD_LIBRARY_PATH:-}" \
        --set SSL_CERT_FILE "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" \
        --set NIX_SSL_CERT_FILE "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
    '';

    # Enhanced init function that handles Python requirements
    pythonInitFn = ''
      ${initFn}

      # Install custom Python requirements if present
      ${lib.optionalString enablePip ''
      if [[ -f "${requirementsPath}" ]]; then
        info "Installing custom Python requirements from ${requirementsPath}..."
        ${pythonEnv}/bin/pip install --quiet -r "${requirementsPath}" || warn "Failed to install some requirements"
      fi
      ''}
    '';

    # Enhanced Docker config with Python environment
    pythonDockerConfig = lib.recursiveUpdate {
      Env = [
        "PATH=${pythonEnv}/bin:/bin:/usr/bin"
        "PYTHONPATH=${pythonSitePackages}"
        "PYTHONDONTWRITEBYTECODE=1"
        "PYTHONUNBUFFERED=1"
      ];
    } dockerConfig;

    # Create the container module
    containerModule = mkContainerModule {
      inherit name version envVars envVarsWithSecrets paths user;
      inherit imageName imageTag;
      inherit validateFn configFn initEveryBoot runCmd;
      initFn = pythonInitFn;

      # Pass through two-phase lifecycle parameters
      prepopulateFn = combinedPrepopulateFn;
      inherit prepopulateFiles prepopulateDirs runtimeDirs;
      inherit activateFn enableStateTracking perContainerHelpers;

      systemDeps = systemDeps ++ [
        pkgs.cacert
        pkgs.stdenv.cc.cc.lib
      ];

      runtimeBinDeps = pythonRuntimeBins;
      extraDeps = extraDeps;

      dockerConfig = pythonDockerConfig;
      inherit exposedPorts;
      volumes = volumes ++ [
        (builtins.dirOf requirementsPath)  # Allow mounting requirements
      ];

      # Forward health config so the base entrypoint wrapper can layer
      # firestream-healthd onto the python entrypoint wrapper. base.nix wraps
      # finalEntrypoint = if health.enable then healthWrapper else
      # innerEntrypoint, where innerEntrypoint already includes pythonEntrypointWrapper.
      inherit health;

      entrypointWrapper = pythonEntrypointWrapper;

      inherit customScripts;
      devShellPackages = devShellPackages ++ [
        pythonEnv
        pkgs.uv
      ];
      devShellHook = ''
        export PYTHONPATH="${pythonSitePackages}"
        echo "Python: ${python}/bin/python"
        echo "Virtual environment: ${pythonEnv}"
        ${devShellHook}
      '';
    };

  in containerModule // {
    # Add Python-specific attributes
    inherit pythonEnv python pythonSitePackages;

    # Export Python-specific config
    config = (containerModule.config or {}) // {
      inherit compileByteCode pythonPrepopulateFn;
    };

    # Override meta with Python info
    meta = containerModule.meta // {
      pythonVersion = python.version;
      byteCodePrecompiled = compileByteCode;
    };
  };
}
