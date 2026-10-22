# Container Module Factory
# Copyright Firestream. MIT License.
#
# This module provides mkContainerModule - a factory function that generates
# complete container modules with Docker image building capabilities.
#
# It wraps mkAppModule with Docker-specific concerns:
# - Docker image building via dockerTools.buildLayeredImage
# - Development shell generation
# - Runtime environment bundling
#
# Usage:
#   let
#     container = mkContainerModule {
#       name = "myapp";
#       version = "1.0.0";
#       systemDeps = [ pkgs.curl pkgs.git ];
#       runtimeBinDeps = [ pkgs.coreutils ];
#       validateFn = "...";
#       configFn = "...";
#       initFn = "...";
#       runCmd = "myapp --serve";
#     };
#   in container.dockerImage

{ pkgs, lib, mkAppModule, coreLibs, waitForPortPkg, firestreamVibPkg, firestreamHealthdPkg }:

let
  # Import metadata library for SBOM and container metadata generation
  metadataLib = import ../lib/metadata.nix { inherit pkgs lib firestreamVibPkg; };

  # Default Docker configuration
  defaultDockerConfig = {
    WorkingDir = "/opt/firestream";
    User = "1001:1001";
  };

  # Default exposed ports
  defaultExposedPorts = {};

  # Default volumes
  defaultVolumes = {
    "/firestream/data" = {};
  };

in
{
  # Factory function to create container modules
  mkContainerModule = {
    # Basic metadata
    name,
    version ? "1.0.0",

    # Docker image naming (parity-preserving defaults match the historical literals)
    imageName ? "firestream-${name}",
    imageTag ? version,

    # Environment configuration (passed to mkAppModule)
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
    # Forwarded to mkAppModule so chart init containers can source
    # /opt/bitnami/scripts/lib<name>.sh and call helpers like
    # kafka_server_conf_set / airflow_conf_set directly.
    perContainerHelpers ? "",

    # Build-time (prepopulate phase) - passed to mkAppModule
    prepopulateFn ? "",
    prepopulateFiles ? {},
    prepopulateDirs ? [],
    runtimeDirs ? {},              # Declarative directory specifications (preferred)

    # Runtime (activate phase) - passed to mkAppModule
    activateFn ? "",
    enableStateTracking ? true,

    # Container-specific dependencies
    systemDeps ? [],          # System packages for the container (libs, tools)
    runtimeBinDeps ? [],      # Packages that need to be in PATH at runtime
    extraDeps ? [],           # Additional deps for mkAppModule

    # Docker configuration
    dockerConfig ? {},        # Override Docker image config
    exposedPorts ? [],        # List of port numbers to expose
    volumes ? [],             # List of volume paths

    # Optional custom entrypoint wrapper
    entrypointWrapper ? null, # Function: entrypoint -> wrapped entrypoint

    # In-image health/SBOM service configuration (Phase 3).
    # When `health.enable == true`, a one-shot shell wrapper is layered ONTO
    # the existing entrypoint that backgrounds firestream-healthd and then
    # `exec`s the inner entrypoint. When false (the default), the image is
    # byte-identical to a pre-Phase-3 image and no health server is launched.
    health ? {
      enable = false;
      port = 9180;
      readinessCmd = null;
    },

    # Optional additional scripts
    customScripts ? {},       # { name = script; } - additional scripts to include

    # Development shell extras
    devShellPackages ? [],    # Extra packages for dev shell
    devShellHook ? "",        # Additional shell hook for dev shell

  }:
  let
    # Create the application module
    appModule = mkAppModule {
      inherit name version envVars envVarsWithSecrets paths user;
      inherit validateFn configFn initFn initEveryBoot runCmd;
      inherit prepopulateFn prepopulateFiles prepopulateDirs runtimeDirs;
      inherit activateFn enableStateTracking perContainerHelpers;
      extraDeps = extraDeps ++ runtimeBinDeps;
    };

    # MUST KEEP: allRuntimeDeps is used by devShell (line 343)
    # devShell doesn't need passwdFile/groupFile/shadowFile
    allRuntimeDeps = lib.lists.unique (
      appModule.runtimeDeps ++ systemDeps ++ runtimeBinDeps ++ [
        pkgs.bashInteractive
        pkgs.cacert
        pkgs.stdenv.cc.cc.lib
        waitForPortPkg  # Rust-based port checker - available in all containers
        firestreamHealthdPkg  # In-image /healthz, /readyz, /sbom, /metadata server (Phase 2: present, not yet launched)
      ]
    );

    # Create /etc/passwd as a proper Nix derivation (idiomatic approach)
    passwdFile = pkgs.writeTextDir "etc/passwd" ''
root:x:0:0:root:/root:/bin/bash
nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin
${user.name}:x:${toString user.uid}:${toString user.gid}:${user.name}:/home/${user.name}:/bin/bash
'';

    # Create /etc/group as a proper Nix derivation
    groupFile = pkgs.writeTextDir "etc/group" ''
root:x:0:
nobody:x:65534:
${user.group}:x:${toString user.gid}:
'';

    # Create /etc/shadow for completeness (locked passwords)
    shadowFile = pkgs.writeTextDir "etc/shadow" ''
root:!x:::::::
nobody:!x:::::::
${user.name}:!:::::::
'';

    # Get the entrypoint, optionally wrapped by the runtime's custom wrapper
    # (e.g. mkJavaContainerModule's JVM wrapper).
    innerEntrypoint =
      if entrypointWrapper != null
      then entrypointWrapper appModule.scripts.entrypoint
      else appModule.scripts.entrypoint;

    # Phase 3: layer a health-launching wrapper ONTO the existing entrypoint
    # when `health.enable == true`. The wrapper:
    #   1. Honors FIRESTREAM_HEALTHD_DISABLE as an escape hatch (skips healthd).
    #   2. Launches `firestream-healthd` in the background (output -> stderr),
    #      passing `--readiness-cmd` only when configured so the binary's
    #      default behavior is preserved otherwise.
    #   3. exec's the inner entrypoint with its original args so the app stays
    #      PID-target (single-exec PID-1 semantics preserved). Healthd is
    #      reaped by Docker when the container stops.
    # The wrapper binary is named exactly `${name}-entrypoint` so the Docker
    # Entrypoint config (`${finalEntrypoint}/bin/${name}-entrypoint`) stays
    # consistent whether or not health is enabled.
    healthWrapper = pkgs.writeShellScriptBin "${name}-entrypoint" ''
      set -euo pipefail

      # Escape hatch: skip healthd entirely.
      if [ -n "''${FIRESTREAM_HEALTHD_DISABLE:-}" ]; then
        exec "${innerEntrypoint}/bin/${name}-entrypoint" "$@"
      fi

      "${firestreamHealthdPkg}/bin/firestream-healthd" \
        --port "${toString health.port}" \
        --bind 0.0.0.0 \
        --metadata-path /opt/firestream \
        ${lib.optionalString (health.readinessCmd != null)
          "--readiness-cmd ${lib.escapeShellArg health.readinessCmd}"} \
        >&2 2>&1 &

      exec "${innerEntrypoint}/bin/${name}-entrypoint" "$@"
    '';

    finalEntrypoint = if health.enable then healthWrapper else innerEntrypoint;

    # Build custom scripts
    customScriptPackages = lib.mapAttrsToList (
      scriptName: scriptContent:
        pkgs.writeScriptBin scriptName scriptContent
    ) customScripts;

    # ========================================================================
    # SINGLE SOURCE OF TRUTH: imageContents
    # ========================================================================
    # All packages that go into the container, EXCLUDING containerMetadata
    # (added separately to avoid circular dependency).
    #
    # ORDER MATTERS: buildLayeredImage uses list order for layer assignment
    # Keep grouping: system fundamentals → deps → app scripts → config
    imageContents = [
      # Layer group 1: System fundamentals (rarely change)
      passwdFile
      groupFile
      shadowFile
      pkgs.bashInteractive
      pkgs.cacert
      pkgs.stdenv.cc.cc.lib
      waitForPortPkg
      firestreamHealthdPkg  # Phase 2: binary present in /nix/store; entrypoint unchanged.
    ] ++ systemDeps ++ runtimeBinDeps ++ extraDeps ++ [
      # Layer group 2: App scripts (change with code updates)
      appModule.scripts.envDefaults
      appModule.scripts.fileLoader
      appModule.scripts.libHelpers   # libhelpers<name>.sh: top-level helpers
      appModule.scripts.lib          # lib<name>.sh: phase wrappers (sources libHelpers)
      appModule.scripts.genericLibs  # libos.sh/liblog.sh/... : Bitnami-compat shims (source libHelpers)
      appModule.scripts.appEnv       # <name>-env.sh : Bitnami-compat env-setup shim for chart probes/jobs
      finalEntrypoint           # CRITICAL: was missing from metadataEnv
      appModule.scripts.setup
      appModule.scripts.run
    ] ++ customScriptPackages   # CRITICAL: was missing from metadataEnv
    ++ [
      # Layer group 3: Config templates (change frequently)
      appModule.prepopulatedEnv
    ];

    # symlinkJoin is lighter-weight than buildEnv
    # Only purpose: give exportReferencesGraph a single derivation whose closure = imageContents
    contentsForMetadata = pkgs.symlinkJoin {
      name = "${name}-metadata-source";
      paths = imageContents;
    };

    # Generate container metadata files (SBOM, closure, etc.)
    # These are generated at Nix build time and included in the container image
    #
    # NOTE: We use contentsForMetadata as mainDrv to capture the FULL container closure,
    # including all system dependencies, libraries, and runtime tools.
    # This ensures SBOM accurately reflects actual container contents.
    containerMetadata = metadataLib.mkContainerMetadata {
      inherit name version;
      mainDrv = contentsForMetadata;  # Uses same packages as container
      exposedPorts = exposedPorts;
      user = "${toString user.uid}:${toString user.gid}";
      workdir = paths.base;
      flakeUri = builtins.getEnv "FLAKE_URI";
      flakeRevision = builtins.getEnv "GIT_COMMIT_HASH";
    };

    # Build exposed ports config
    portsConfig = lib.listToAttrs (
      map (port: {
        name = "${toString port}/tcp";
        value = {};
      }) exposedPorts
    );

    # Build volumes config.
    #
    # Drop any declared volume path that is a STRICT CHILD of another declared
    # volume (or of a default volume). A nested image VOLUME is honoured by
    # containerd/CRI as an anonymous *ephemeral* mount that SHADOWS the parent
    # volume — so when a chart binds a PVC at the parent (e.g. kafka mounts its
    # data PVC at /firestream/kafka), the nested image VOLUME at
    # /firestream/kafka/data masks the PVC subdir and silently discards anything
    # written there (Kafka's meta.properties) on every container restart. Only
    # the top-most paths should be declared as image volumes; children then live
    # on whatever volume is mounted at the parent.
    declaredVolumePaths = lib.unique (
      (lib.attrNames defaultVolumes) ++ [ paths.data paths.logs ] ++ volumes
    );
    isStrictChildOfAnother = p:
      lib.any (q: q != p && lib.hasPrefix (q + "/") p) declaredVolumePaths;
    volumesConfig = lib.listToAttrs (
      map (path: {
        name = path;
        value = {};
      }) (lib.filter (p: !(isStrictChildOfAnother p)) ([ paths.data paths.logs ] ++ volumes))
    );

    # MUST KEEP: runtimeEnv is exported and used by:
    # - test-containers.nix:84-88 (test asserts testModule ? runtimeEnv)
    # - metadata.nix:136 (mkMetadataForContainer uses containerModule.runtimeEnv)
    # - Referenced in module return value
    runtimeEnv = pkgs.buildEnv {
      name = "${name}-runtime-env";
      paths = imageContents;
      pathsToLink = [ "/bin" "/lib" "/lib64" "/opt" "/share" "/etc" ];
    };

    # Docker image
    dockerImage = pkgs.dockerTools.buildLayeredImage {
      name = imageName;
      tag = imageTag;
      maxLayers = 100;  # Modern Docker supports 128, use 100 for fine-grained caching

      # Contents = imageContents + metadata
      # imageContents is the single source of truth for what goes in the container
      # containerMetadata is added separately (depends on imageContents, so can't be part of it)
      contents = imageContents ++ [ containerMetadata ];

      # Fix ownership for non-root user (runs as fakeroot during image build)
      # Create all runtime directories at build time so container can run as non-root
      # Note: /etc/passwd, /etc/group, /etc/shadow are created as Nix derivations above
      fakeRootCommands = ''
        # Create system /tmp directory with sticky bit (required for mktemp, etc.)
        mkdir -p ./tmp
        chmod 1777 ./tmp

        # CRITICAL: Resolve /etc symlinks to actual files
        # Docker health checks fail with "path escapes from parent" if /etc/* are symlinks
        # to Nix store paths (e.g., /etc/passwd -> /nix/store/xxx-passwd/etc/passwd)
        for etc_file in ./etc/passwd ./etc/group ./etc/shadow; do
          if [ -L "$etc_file" ]; then
            target=$(readlink -f "$etc_file")
            rm "$etc_file"
            cp "$target" "$etc_file"
            echo "Resolved /etc symlink: $etc_file"
          fi
        done

        # Create all runtime directories from runtimeDirs schema
        ${lib.concatMapStringsSep "\n" (dirSpec: ''
          mkdir -p .${dirSpec.path}
          chown ${toString user.uid}:${toString user.gid} .${dirSpec.path}
          chmod ${dirSpec.mode} .${dirSpec.path}
        '') (lib.mapAttrsToList (name: spec: {
          path = spec.path;
          mode = spec.mode or "0755";
        }) (appModule.config.runtimeDirs or {}))}

        # Helper function to resolve symlinks to real files/dirs
        resolve_symlink() {
          local path="$1"
          if [ -L "$path" ]; then
            local target=$(readlink -f "$path")
            rm "$path"
            cp -r "$target" "$path"
            echo "Resolved symlink: $path"
          fi
        }

        # OPTIMIZED: Only resolve app-specific directories (not system paths)
        # /bin, /lib remain as symlinks; /etc is resolved above for health check compatibility
        for base_dir in ./opt ./firestream; do
          app_dir="$base_dir/${name}"
          resolve_symlink "$app_dir"
        done

        # Bitnami chart compat: the upstream charts source helper scripts
        # from `/opt/bitnami/scripts/lib<name>.sh`. Mirror the firestream
        # scripts at the Bitnami path so chart command/args overrides Just
        # Work without patching every chart's templates.
        #
        # We deliberately do NOT symlink /opt/bitnami/${name} → /opt/firestream/${name}:
        # several Bitnami charts mount configmaps/secrets inside that path
        # (e.g. `/opt/bitnami/airflow/airflow.cfg`), and Kubernetes can't
        # overlay mounts onto a symlink target. Instead we create an empty
        # writable directory there so volume mounts have a real target.
        if [ -d "./opt/firestream/scripts" ] && [ ! -e "./opt/bitnami/scripts" ]; then
          mkdir -p ./opt/bitnami
          ln -s /opt/firestream/scripts ./opt/bitnami/scripts
        fi
        mkdir -p ./opt/bitnami/${name}

        # Ensure home directory exists
        mkdir -p ./home/${user.name}

        # OPTIMIZED: Batch ownership with single chown -R call
        chown -R ${toString user.uid}:${toString user.gid} \
          ./opt/${name} \
          ./firestream/${name} \
          ./home/${user.name} \
          2>/dev/null || true

        # OPTIMIZED: Use xargs for batch chmod (10x faster than -exec per file)
        find ./opt/${name} -type d -print0 2>/dev/null | xargs -0 -r chmod 755 2>/dev/null || true
        find ./opt/${name} -type f -print0 2>/dev/null | xargs -0 -r chmod 644 2>/dev/null || true
        find ./firestream/${name} -type d -print0 2>/dev/null | xargs -0 -r chmod 755 2>/dev/null || true
        find ./firestream/${name} -type f -print0 2>/dev/null | xargs -0 -r chmod 644 2>/dev/null || true
      '';

      # Enable fakeroot for ownership changes
      enableFakechroot = true;

      config = lib.recursiveUpdate (lib.recursiveUpdate defaultDockerConfig {
        Env = [
          "PATH=/bin:/usr/bin"
          "HOME=/home/${user.name}"
          "LD_LIBRARY_PATH=/lib:/lib64:${pkgs.stdenv.cc.cc.lib}/lib"
          "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
          "NIX_SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        ];
        WorkingDir = paths.base;
        Entrypoint = [ "${finalEntrypoint}/bin/${name}-entrypoint" ];
        ExposedPorts = lib.recursiveUpdate defaultExposedPorts portsConfig;
        Volumes = lib.recursiveUpdate defaultVolumes volumesConfig;
        User = "${toString user.uid}:${toString user.gid}";
      }) dockerConfig;
    };

    # Development shell
    devShell = pkgs.mkShell {
      buildInputs = allRuntimeDeps ++ devShellPackages ++ [
        pkgs.docker
        pkgs.docker-compose
      ];

      shellHook = ''
        echo "Firestream ${name} Development Environment"
        echo "Version: ${version}"
        echo ""
        echo "Available commands:"
        echo "  nix build .#dockerImage    - Build the Docker image"
        echo "  nix develop               - Enter development shell"
        echo ""
        ${devShellHook}
      '';
    };

  in {
    # Module metadata
    meta = {
      inherit name version;
      description = "Firestream ${name} container module";
      maintainer = "Firestream Contributors";
      license = "MIT";
    };

    # All runtime dependencies
    runtimeDeps = allRuntimeDeps;

    # Generated scripts (from appModule plus any custom)
    scripts = appModule.scripts // {
      custom = customScriptPackages;
    };

    # Complete runtime environment
    inherit runtimeEnv;

    # Docker image
    inherit dockerImage;

    # Development shell
    inherit devShell;

    # Configuration used (for introspection)
    config = appModule.config // {
      inherit systemDeps runtimeBinDeps exposedPorts volumes;
      inherit prepopulateFn prepopulateFiles prepopulateDirs runtimeDirs activateFn enableStateTracking;
    };

    # Volume hints for orchestration (from appModule)
    inherit (appModule) volumeHints;

    # Expose prepopulated environment and state library from appModule
    inherit (appModule) prepopulatedEnv stateLib;

    # Expose app module for advanced usage
    inherit appModule;

    # ========================================================================
    # Fleet Manifest Support
    # ========================================================================

    # Expose container metadata derivation for fleet SBOM aggregation
    # This is the derivation containing metadata.json, sbom-cyclonedx.json, etc.
    metadata = containerMetadata;

    # Expose package list for fleet-level source introspection
    # Uses allRuntimeDeps (real Nix packages with .src attributes),
    # NOT imageContents (which includes generated passwd/script derivations
    # that can't be interpolated into string contexts)
    packageList = allRuntimeDeps;

    # Tag for dynamic filtering in fleet manifest collection
    # Used by collectArtifacts to identify containers without hardcoding names
    isFirestreamContainer = true;
  };
}
