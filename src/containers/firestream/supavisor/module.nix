# Supavisor Container Module
# Copyright Firestream. MIT License.
#
# Supavisor is an Elixir release with a Rust NIF (native/pgparser), built
# from source with mkMixRelease and run with mkBeamContainerModule. Every boot
# runs the upstream compose command before the server: the migrations, then
# the upstream docker/volumes/pooler/pooler.exs, which creates the pooler
# tenant when it does not exist.

{ pkgs
, lib
, firestream

, version ? "2.9.13"

, paths ? {
    base = "/opt/firestream/supavisor";
    conf = "/opt/firestream/supavisor/config";
    data = "/firestream/supavisor/data";
    logs = "/opt/firestream/supavisor/logs";
  }

, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 4000 5432 6543 ]
, imageName ? "firestream-supabase-supavisor"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  release = firestream.mkMixRelease {
    pname = "supavisor";
    inherit version;
    src = pkgs.fetchFromGitHub {
      owner = "supabase";
      repo = "supavisor";
      rev = "5a51fbfadaa6d6286c1fee0a95d4c629e81a2750";
      hash = "sha256-rI4RC4FDpFQD4ezNn9ntOznrr/xnpuHZldSCNR3C4D8=";
    };
    # NOTE: OTP 27, as upstream pins. The spike [FIRE-010] did not test 28.
    erlang = "erlang_27";
    elixir = "elixir_1_18";
    mixDeps = ./mix-deps.nix;
    nif = { cargoRoot = "native"; cargoHash = "sha256-gr/w9kdU4qv/Zjzw+8XE++6WNLbgC2SW/flDH25Bslk="; };
  };

  poolerSeed = "${firestream.supabase.sources.dockerVolumes}/docker/volumes/pooler/pooler.exs";
in
firestream.mkBeamContainerModule {
  name = "supavisor";
  inherit version paths envVars envVarsWithSecrets;
  inherit imageName imageTag exposedPorts health release;

  validateFn = ''
    for v in POSTGRES_PASSWORD SECRET_KEY_BASE VAULT_ENC_KEY API_JWT_SECRET METRICS_JWT_SECRET; do
      [[ -n "''${!v:-}" ]] || print_validation_error "$v is required"
    done
  '';

  # WARNING: the password goes into a URL unescaped, as upstream does. A
  # password with URL-reserved characters needs an explicit DATABASE_URL.
  configFn = ''
    if [[ -z "''${DATABASE_URL:-}" ]]; then
      export DATABASE_URL="ecto://supabase_admin:''${POSTGRES_PASSWORD}@''${POSTGRES_HOST}:''${POSTGRES_PORT}/_supabase"
    fi
  '';

  preStart = ''
    "$RELEASE_BIN" eval 'Supavisor.Release.migrate()'
    "$RELEASE_BIN" eval "$(cat ${poolerSeed})"
  '';

  enableStateTracking = false;

  # NOTE: the release env.sh names the node `supavisor@$NODE_IP`, else
  # `supavisor@$(hostname -f)`.
  runtimeBinDeps = [ pkgs.hostname ];

  user = { name = "supavisor"; group = "supavisor"; uid = 1001; gid = 1001; };
}
