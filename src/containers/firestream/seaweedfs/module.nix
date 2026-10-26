# SeaweedFS Container Module - Using Firestream Factories
# Copyright Firestream. MIT License.
#
# Defines the SeaweedFS container using mkContainerModule. SeaweedFS is the
# Firestream object store (9th supported app): a single Go binary (`weed`)
# exposing an S3-compatible API.
#
# Deliberately MINIMAL. SeaweedFS is NOT a Bitnami chart — its upstream Helm
# chart invokes `weed` directly via a `command:` block and never sources
# /opt/bitnami/scripts/lib*.sh. So the Firestream Bitnami-compat machinery is
# omitted entirely: no perContainerHelpers, no validate/config/init scripts, no
# extraEnvVars path-remap dance. The container is simply "weed on PATH".
#
# The `weed` binary is built by bin/nix/firestream/packages/seaweedfs-weed.nix
# and threaded in here as `seaweedfsWeedPkg` via the flake-module's
# extraFactoryArgs (mirroring superset's waitForPortPkg injection).
#
# Usage:
#   seaweedfsModule = import ./module.nix {
#     inherit pkgs lib firestream seaweedfsWeedPkg;
#   };

{ pkgs
, lib
, firestream

# Pinned version (single-version app). Default mirrors options.nix.
, version ? "4.36-17-g5797fb24e"

# The SeaweedFS `weed` package (from the Firestream packages index), placed on
# PATH via runtimeBinDeps. Required — supplied by the flake-module.
, seaweedfsWeedPkg

# Externalized core-surface config. Defaults below equal options.nix literals so
# the direct-import path and evalContainer (which passes the same values from
# options.nix) yield identical factory args.

, paths ? {
    base = "/opt/firestream/seaweedfs";
    conf = "/opt/firestream/seaweedfs/config";
    data = "/firestream/seaweedfs/data";
    logs = "/opt/firestream/seaweedfs/logs";
  }

, envVars ? {
    AWS_ACCESS_KEY_ID = "";
    AWS_SECRET_ACCESS_KEY = "";
  }

# Variables supporting _FILE suffix for Docker/K8s secrets
, envVarsWithSecrets ? [
    "AWS_ACCESS_KEY_ID"
    "AWS_SECRET_ACCESS_KEY"
  ]

# s3, master, filer, volume, metrics
, exposedPorts ? [ 8333 9333 8888 8080 9324 ]

# Image naming passthrough (parity defaults).
, imageName ? "firestream-seaweedfs"
, imageTag ? version
}:

let
  # System dependencies (shared libs/tools available in the container).
  systemDeps = with pkgs; [
    cacert openssl
    coreutils gnugrep gnused gawk findutils which
    procps
  ];

  # Runtime binary deps (on PATH at runtime). `weed` is the headline binary.
  # `wget` is required by the chart's post-install bucket hook
  # (templates/shared/post-install-bucket-hook.yaml), whose readiness gate runs
  # `wget -q --spider http://<master>/cluster/status` before creating buckets.
  # Without it the hook loops "command not found" for 5m, fails, and atomic
  # deploys roll back. The hook runs in this image (master.image).
  runtimeBinDeps = [ seaweedfsWeedPkg ] ++ (with pkgs; [
    coreutils bash gnused gnugrep gawk findutils which
    wget
  ]);

in firestream.mkContainerModule {
  name = "seaweedfs";
  inherit version;

  inherit paths envVars envVarsWithSecrets;

  # Image naming passthrough.
  inherit imageName imageTag;

  # Declarative directory schema. SeaweedFS keeps all master/volume/filer state
  # under a single -dir; one persistent data directory suffices.
  runtimeDirs = {
    data = {
      path = "/firestream/seaweedfs/data";
      type = "data";
      persistence = "persistent";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "SeaweedFS data directory (master/volume/filer state)";
    };
  };

  # Standalone / docker-compose startup command. All-in-one single pod:
  # master + volume + filer + s3 in one process, S3 on 8333, bound to all
  # interfaces. The Helm chart overrides this via its own `command:` block, so
  # this default only governs direct `docker run` / compose use.
  # WARNING: the entrypoint already runs `exec ${runCmd}`. A leading `exec`
  # here makes the shell run `exec exec weed`, which exits 127.
  runCmd = ''
    weed server -dir=/firestream/seaweedfs/data -master -volume -filer -s3 -ip.bind=0.0.0.0
  '';

  inherit systemDeps runtimeBinDeps;

  inherit exposedPorts;
  volumes = [ "/firestream/seaweedfs" ];

  user = { name = "seaweedfs"; group = "seaweedfs"; uid = 1001; gid = 1001; };

  # Development shell extras
  devShellPackages = with pkgs; [ docker docker-compose ];
  devShellHook = ''
    echo "SeaweedFS Version: ${version}"
    echo "weed Binary: ${seaweedfsWeedPkg}/bin/weed"
    echo ""
    echo "Build commands:"
    echo "  nix build .#seaweedfs       - Build the Docker image"
    echo "  docker load < result        - Load image into Docker"
  '';
}
