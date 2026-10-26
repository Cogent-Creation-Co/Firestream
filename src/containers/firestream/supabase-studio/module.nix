# Supabase Studio Container Module
# Copyright Firestream. MIT License.
#
# The Next.js standalone server that ./package.nix builds with pnpm2nix,
# wrapped by mkNodeContainerModule. Studio talks to postgres-meta
# (STUDIO_PG_META_URL) and to the gateway (SUPABASE_URL); it keeps only the
# snippet and function files under paths.data.
#
# pnpm2nix (the patched builder) and fenix come from the flake-module
# through extraFactoryArgs.

{ pkgs
, lib
, firestream
, pnpm2nix
, fenix
, version ? "2026.10.12"
, paths ? {
    base = "/opt/firestream/studio";
    conf = "/opt/firestream/studio/config";
    data = "/firestream/studio/data";
    logs = "/opt/firestream/studio/logs";
  }
, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 3000 ]
, imageName ? "firestream-supabase-studio"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  studio = import ./package.nix { inherit pkgs lib pnpm2nix fenix; };
in
firestream.mkNodeContainerModule {
  name = "studio";
  inherit version paths;
  inherit envVars envVarsWithSecrets exposedPorts imageName imageTag health;
  inherit (studio) nodejs;
  nodePackage = studio;

  validateFn = ''
    for var in POSTGRES_PASSWORD PG_META_CRYPTO_KEY SUPABASE_ANON_KEY SUPABASE_SERVICE_KEY AUTH_JWT_SECRET; do
      [[ -n "''${!var:-}" ]] || print_validation_error "$var is required"
    done
  '';

  # NOTE: not HOSTNAME in the image env: Docker and Kubernetes always set
  # HOSTNAME to the container name, and Next.js then listens on the pod
  # address only, so a probe to 127.0.0.1 fails.
  configFn = ''
    export HOSTNAME="''${STUDIO_BIND_ADDRESS:-0.0.0.0}"
    # Without a writable volume the folders stay absent, and only the snippet
    # and function pages fail.
    mkdir -p "$SNIPPETS_MANAGEMENT_FOLDER" "$EDGE_FUNCTIONS_MANAGEMENT_FOLDER" 2>/dev/null \
      || warn "cannot create the snippet and function folders"
  '';

  runCmd = "supabase-studio";

  volumes = [ "/firestream/studio/data" ];

  # The health.readinessCmd tools; healthd runs with the image PATH.
  systemDeps = with pkgs; [ coreutils gnugrep ];

  enableStateTracking = false;

  user = { name = "studio"; group = "studio"; uid = 1001; gid = 1001; };
}
