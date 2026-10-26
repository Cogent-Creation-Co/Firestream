# Elixir/BEAM Container Module Factory
# Copyright Firestream. MIT License.
#
# This module provides mkBeamContainerModule - a specialized factory for
# Elixir releases built with mkMixRelease (bin/nix/firestream/beam).
#
# Features:
# - Runs `bin/<release> start` with a writable RELEASE_TMP
# - preStart: release commands (migrate, seed) before the server, every boot
# - A per-boot node cookie unless RELEASE_COOKIE is supplied
# - UTF-8 locale for the VM
#
# Usage:
#   let
#     release = firestream.mkMixRelease { pname = "realtime"; ... };
#     container = mkBeamContainerModule {
#       name = "realtime";
#       version = "2.143.3";
#       inherit release;
#       preStart = ''
#         "$RELEASE_BIN" eval 'Realtime.Release.migrate()'
#       '';
#     };
#   in container.dockerImage

{ pkgs, lib, mkContainerModule }:

{
  mkBeamContainerModule = {
    # Basic metadata
    name,
    version ? "1.0.0",
    imageName ? "firestream-${name}",
    imageTag ? version,

    # Release configuration (required)
    release,                        # The release from mkMixRelease
    releaseName ? name,             # bin/<releaseName> inside the release

    # Shell run before `start` on every boot, with RELEASE_BIN set. It runs
    # in its own process, so it sees the env but not the helper functions.
    preStart ? "",

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
    activateFn ? "",
    perContainerHelpers ? "",
    enableStateTracking ? true,

    # Container dependencies
    systemDeps ? [],
    runtimeBinDeps ? [],
    extraDeps ? [],

    # Docker configuration
    dockerConfig ? {},
    exposedPorts ? [],
    volumes ? [],
    runtimeDirs ? {},

    health ? {
      enable = false;
      port = 9180;
      readinessCmd = null;
    },

    customScripts ? {},
    devShellPackages ? [],
    devShellHook ? "",
  }:
  let
    releaseBin = "${release}/bin/${releaseName}";
    releaseTmp = "${paths.base}/tmp";

    beamEnvVars = {
      # NOTE: the release writes its runtime config and vm.args here on every
      # start. The default is inside the release, which is read-only in the
      # Nix store.
      RELEASE_TMP = releaseTmp;
      LANG = "C.UTF-8";
      ELIXIR_ERL_OPTIONS = "+fnu";
    } // envVars;

    # NOTE: a fresh cookie per boot is safe for a single node. A cluster
    # needs one shared cookie, so RELEASE_COOKIE from the environment wins.
    cookieFn = ''
      if [[ -z "''${RELEASE_COOKIE:-}" ]]; then
        RELEASE_COOKIE="$(head -c 32 /dev/urandom | base32 | tr -d '=\n')"
        export RELEASE_COOKIE
        debug "Generated a node cookie for this boot; set RELEASE_COOKIE to cluster nodes"
      fi
    '';

    startScript = pkgs.writeShellScript "${name}-start" ''
      set -euo pipefail
      export RELEASE_BIN=${releaseBin}
      ${preStart}
      exec "$RELEASE_BIN" start
    '';

    beamRuntimeBins = [
      release
      pkgs.coreutils
      pkgs.gnused
      pkgs.gnugrep
      pkgs.gawk
      pkgs.bash
    ] ++ runtimeBinDeps;

    containerModule = mkContainerModule {
      inherit name version paths user;
      inherit imageName imageTag;
      envVars = beamEnvVars;
      inherit envVarsWithSecrets;

      inherit validateFn configFn initFn;
      activateFn = cookieFn + activateFn;
      runCmd = "${startScript}";
      inherit enableStateTracking perContainerHelpers;

      runtimeDirs = {
        releaseTmp = {
          path = releaseTmp;
          type = "tmp";
          persistence = "ephemeral";
          mode = "1777";
          description = "Elixir release runtime files (RELEASE_TMP)";
        };
      } // runtimeDirs;

      systemDeps = lib.lists.unique ([ pkgs.cacert ] ++ systemDeps);
      runtimeBinDeps = beamRuntimeBins;
      extraDeps = extraDeps ++ [ release ];

      inherit dockerConfig exposedPorts volumes health customScripts;

      devShellPackages = devShellPackages ++ [
        release.passthru.beamPackages.elixir
        release.passthru.beamPackages.erlang
      ];
      inherit devShellHook;
    };

  in containerModule // {
    inherit release releaseName;

    meta = containerModule.meta // {
      erlangVersion = release.passthru.beamPackages.erlang.version;
      elixirVersion = release.passthru.beamPackages.elixir.version;
    };
  };
}
