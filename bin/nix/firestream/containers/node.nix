# Node.js Container Module Factory
# Copyright Firestream. MIT License.
#
# This module provides mkNodeContainerModule - a specialized factory for
# Node.js applications that integrates with buildNpmPackage.
#
# Features:
# - pnpm as primary package manager (npm fallback)
# - Built-in TypeScript support
# - Automatic NODE_ENV configuration
# - SSL certificate bundling
# - Runtime npm install capability
# - Health check support
#
# Usage:
#   let
#     nodePackage = mkNodePackage { ... };
#     container = mkNodeContainerModule {
#       name = "websocket-server";
#       version = "1.0.0";
#       inherit nodePackage;
#       runCmd = "exec node dist/index.js";
#     };
#   in container.dockerImage

{ pkgs, lib, mkContainerModule }:

let
  # Default Node.js version
  defaultNodejs = pkgs.nodejs_22;

in {
  # Factory function for Node.js container modules
  mkNodeContainerModule = {
    # Basic metadata
    name,
    version ? "1.0.0",

    # Node.js configuration (required)
    nodePackage,                    # The built Node.js package from mkNodePackage
    nodejs ? defaultNodejs,         # Node.js interpreter

    # Environment configuration
    envVars ? {},
    envVarsWithSecrets ? [],

    # Node.js specific configuration
    nodeEnv ? "production",         # NODE_ENV value
    enableRuntimeInstall ? false,   # Allow npm/pnpm install at runtime
    runtimePackagesPath ? "/app/runtime-packages.json",  # Runtime packages spec

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

    # Image naming and in-image health service, forwarded to
    # mkContainerModule. The defaults equal mkContainerModule's own.
    imageName ? "firestream-${name}",
    imageTag ? version,
    health ? { enable = false; port = 9180; readinessCmd = null; },

    # Build-time (prepopulate phase)
    prepopulateFn ? "",
    prepopulateFiles ? {},
    prepopulateDirs ? [],
    runtimeDirs ? {},

    # Runtime (activate phase)
    activateFn ? "",
    enableStateTracking ? true,

    # Health check configuration
    healthCheckPort ? null,         # Port for health check (HTTP)
    healthCheckPath ? "/health",    # Health check endpoint path
    healthCheckInterval ? 30,       # Seconds between health checks

    # Custom scripts and hooks
    customScripts ? {},
    devShellPackages ? [],
    devShellHook ? "",
  }:
  let
    # Node.js-specific helper functions
    nodeHelpers = ''
      ########################
      # Check if Node.js is available and working
      # Globals:
      #   NODE_HOME
      # Arguments:
      #   None
      # Returns:
      #   0 if Node.js is available, 1 otherwise
      #########################
      node_check_installation() {
          if ! command -v node &> /dev/null; then
              error "Node.js not found in PATH"
              return 1
          fi
          debug "Node.js version: $(node --version)"
          debug "npm version: $(npm --version 2>/dev/null || echo 'not available')"
          return 0
      }

      ########################
      # Install runtime packages if specified
      # Globals:
      #   None
      # Arguments:
      #   $1 - Path to packages.json file
      # Returns:
      #   0 on success, 1 on failure
      #########################
      node_install_runtime_packages() {
          local packages_file="''${1:-${runtimePackagesPath}}"

          if [[ -f "$packages_file" ]]; then
              info "Installing runtime packages from $packages_file..."
              local pkg_dir
              pkg_dir="$(dirname "$packages_file")"

              cd "$pkg_dir" || return 1

              if command -v pnpm &> /dev/null; then
                  pnpm install --prod 2>&1 || {
                      warn "pnpm install failed, trying npm..."
                      npm install --production 2>&1 || return 1
                  }
              else
                  npm install --production 2>&1 || return 1
              fi

              cd - > /dev/null || return 1
              info "Runtime packages installed successfully"
          fi
          return 0
      }

      ########################
      # Wait for a Node.js HTTP server to be ready
      # Globals:
      #   None
      # Arguments:
      #   $1 - Port number
      #   $2 - Path (default: /health)
      #   $3 - Timeout in seconds (default: 60)
      # Returns:
      #   0 if server is ready, 1 on timeout
      #########################
      node_wait_for_http() {
          local port="$1"
          local path="''${2:-/health}"
          local timeout="''${3:-60}"
          local start_time
          start_time=$(date +%s)

          info "Waiting for HTTP server on port $port$path (timeout: ''${timeout}s)..."

          while true; do
              if curl -sf "http://localhost:$port$path" > /dev/null 2>&1; then
                  info "HTTP server is ready"
                  return 0
              fi

              local current_time
              current_time=$(date +%s)
              local elapsed=$((current_time - start_time))

              if [[ $elapsed -ge $timeout ]]; then
                  error "Timeout waiting for HTTP server on port $port"
                  return 1
              fi

              sleep 1
          done
      }
    '';

    # Node.js-specific system dependencies
    nodeSystemDeps = [
      nodejs
      pkgs.cacert
      pkgs.stdenv.cc.cc.lib
      pkgs.curl  # For health checks
    ] ++ lib.optionals enableRuntimeInstall [
      pkgs.nodePackages.pnpm
    ];

    # Merge with user-provided systemDeps
    allSystemDeps = lib.lists.unique (nodeSystemDeps ++ systemDeps);

    # Node.js runtime binary dependencies
    nodeRuntimeBins = [ nodePackage nodejs ] ++ runtimeBinDeps;

    # Combine base prepopulateFn with Node.js-specific setup
    combinedPrepopulateFn = ''
      ${prepopulateFn}

      # Node.js-specific prepopulation
      # Verify the Node.js package is correctly installed
      if [[ ! -d "${nodePackage}/lib" ]]; then
        warn "Node.js package may not be correctly structured"
      fi
    '';

    # Enhanced initFn with runtime package installation
    nodeInitFn = ''
      ${initFn}

      ${lib.optionalString enableRuntimeInstall ''
      # Install runtime packages if present
      node_install_runtime_packages "${runtimePackagesPath}" || warn "Failed to install runtime packages"
      ''}
    '';

    # Enhanced validateFn with Node.js helpers
    combinedValidateFn = ''
      ${nodeHelpers}

      # Validate Node.js installation
      node_check_installation || return 1

      ${validateFn}
    '';

    # Merge Node.js environment variables with user-provided ones
    nodeEnvVars = {
      # Node.js runtime
      NODE_ENV = nodeEnv;
      NODE_HOME = "${nodejs}";

      # SSL certificates
      NODE_EXTRA_CA_CERTS = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

      # Disable npm update check
      NO_UPDATE_NOTIFIER = "1";
      npm_config_update_notifier = "false";

      # pnpm configuration (if used)
      PNPM_HOME = "/opt/firestream/${name}/.pnpm";
    } // envVars;

    # Create Node.js entrypoint wrapper
    nodeEntrypointWrapper = entrypoint: pkgs.runCommand "${name}-node-entrypoint" {
      nativeBuildInputs = [ pkgs.makeWrapper ];
    } ''
      mkdir -p $out/bin
      makeWrapper ${entrypoint}/bin/${name}-entrypoint $out/bin/${name}-entrypoint \
        --set NODE_ENV "${nodeEnv}" \
        --set PATH "${lib.makeBinPath nodeRuntimeBins}:$PATH" \
        --set NODE_EXTRA_CA_CERTS "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" \
        --set LD_LIBRARY_PATH "${lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib ]}:''${LD_LIBRARY_PATH:-}" \
        --set SSL_CERT_FILE "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" \
        --set NIX_SSL_CERT_FILE "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
    '';

    # Enhanced Docker config with Node.js environment
    nodeDockerConfig = lib.recursiveUpdate {
      Env = [
        "PATH=${nodePackage}/bin:${nodejs}/bin:/bin:/usr/bin"
        "NODE_ENV=${nodeEnv}"
        "NODE_EXTRA_CA_CERTS=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      ];
      # Health check if port specified
      ${if healthCheckPort != null then "Healthcheck" else null} = {
        Test = [ "CMD" "curl" "-sf" "http://localhost:${toString healthCheckPort}${healthCheckPath}" ];
        Interval = healthCheckInterval * 1000000000;  # Convert to nanoseconds
        Timeout = 5000000000;    # 5 seconds
        Retries = 3;
        StartPeriod = 10000000000;  # 10 seconds
      };
    } dockerConfig;

    # Create the container module
    containerModule = mkContainerModule {
      inherit name version paths user;
      envVars = nodeEnvVars;
      inherit envVarsWithSecrets;

      validateFn = combinedValidateFn;
      inherit configFn runCmd;
      initFn = nodeInitFn;

      # Pass through two-phase lifecycle parameters
      prepopulateFn = combinedPrepopulateFn;
      inherit prepopulateFiles prepopulateDirs runtimeDirs;
      inherit activateFn enableStateTracking perContainerHelpers;

      systemDeps = allSystemDeps;
      runtimeBinDeps = nodeRuntimeBins;
      extraDeps = extraDeps ++ [ nodePackage ];

      dockerConfig = nodeDockerConfig;
      inherit exposedPorts volumes;
      inherit imageName imageTag health;

      entrypointWrapper = nodeEntrypointWrapper;

      inherit customScripts;
      devShellPackages = devShellPackages ++ [
        nodejs
        pkgs.nodePackages.pnpm
        pkgs.nodePackages.typescript
        pkgs.nodePackages.ts-node
      ];
      devShellHook = ''
        export NODE_ENV="${nodeEnv}"
        export NODE_EXTRA_CA_CERTS="${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        echo "Node.js Development Environment"
        echo "  Node.js: $(node --version)"
        echo "  pnpm: $(pnpm --version 2>/dev/null || echo 'not available')"
        echo "  TypeScript: $(tsc --version 2>/dev/null || echo 'not available')"
        echo "  Package: ${nodePackage.name or name}"
        echo ""
        ${devShellHook}
      '';
    };

  in containerModule // {
    # Add Node.js-specific attributes
    inherit nodePackage nodejs nodeEnv;

    # Export Node.js-specific config
    config = (containerModule.config or {}) // {
      inherit enableRuntimeInstall runtimePackagesPath;
      inherit healthCheckPort healthCheckPath healthCheckInterval;
    };

    # Override meta with Node.js info
    meta = containerModule.meta // {
      nodeVersion = nodejs.version;
      nodeEnv = nodeEnv;
      runtimeInstallEnabled = enableRuntimeInstall;
      hasHealthCheck = healthCheckPort != null;
    };
  };
}
