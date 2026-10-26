# Supabase API gateway (Envoy) Container Module
# Copyright Firestream. MIT License.
#
# Envoy with the Supabase self-hosted route, API key and sb_ key translation
# configuration. The config is the upstream `docker/volumes/api/envoy` set
# from `firestream.supabase.sources.dockerVolumes`, so an upstream sync is a
# pin bump:
#   1. Build time: render-templates.py replaces the hard-coded service
#      addresses with ENVOY_* placeholders, and the result is checked with
#      `envoy --mode validate`.
#   2. Boot: the entrypoint fills in the keys and addresses from env and
#      writes the config to ENVOY_RUN_DIR.

{ pkgs
, lib
, firestream

, version ? "1.39.1"

, paths ? {
    base = "/opt/firestream/envoy";
    conf = "/opt/firestream/envoy/config";
    data = "/firestream/envoy/data";
    logs = "/opt/firestream/envoy/logs";
  }

, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 8000 ]
, imageName ? "firestream-supabase-envoy"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

let
  envoy = firestream.packages.envoy;

  upstream = "${firestream.supabase.sources.dockerVolumes}/docker/volumes/api/envoy";

  templateVars = [
    "ANON_KEY"
    "ANON_KEY_ASYMMETRIC"
    "SERVICE_ROLE_KEY"
    "SERVICE_ROLE_KEY_ASYMMETRIC"
    "SUPABASE_PUBLISHABLE_KEY"
    "SUPABASE_SECRET_KEY"
    "SUPABASE_PUBLIC_URL"
    "DASHBOARD_BASIC_AUTH"
    "ENVOY_RUN_DIR"
    "ENVOY_HTTP_PORT"
    "ENVOY_REALTIME_TENANT_HOST"
  ] ++ lib.concatMap (s: [ "ENVOY_${s}_HOST" "ENVOY_${s}_PORT" ])
    [ "AUTH" "REST" "REALTIME" "STORAGE" "FUNCTIONS" "META" "STUDIO" ];

  # NOTE: an explicit variable list, because the Lua filters hold `$` in
  # their patterns and a bare envsubst would erase them.
  envsubstFormat = lib.concatMapStringsSep " " (v: "\$${v}") templateVars;

  # Renders the templates in $1 into $2 with the current env.
  renderConfig = pkgs.writeShellScript "supabase-envoy-render" ''
    set -euo pipefail
    for f in envoy cds lds; do
      ${pkgs.gettext}/bin/envsubst ${lib.escapeShellArg envsubstFormat} \
        < "$1/$f.template.yaml" > "$2/$f.yaml.tmp"
      mv "$2/$f.yaml.tmp" "$2/$f.yaml"
    done
  '';

  templates = pkgs.runCommand "supabase-envoy-templates"
    {
      nativeBuildInputs = [ pkgs.python3 pkgs.yq-go pkgs.openssl envoy ];
    } ''
    mkdir -p $out
    python3 ${./render-templates.py} ${upstream} $out

    # Validate a full render. Envoy's validate mode does not load dynamic
    # resources, so the check merges LDS and CDS into static_resources.
    mkdir check
    export ANON_KEY=anon SERVICE_ROLE_KEY=service SUPABASE_PUBLISHABLE_KEY=sb_publishable_x \
      SUPABASE_SECRET_KEY=sb_secret_x ANON_KEY_ASYMMETRIC=anon-es SERVICE_ROLE_KEY_ASYMMETRIC=service-es \
      SUPABASE_PUBLIC_URL=http://localhost:8000 \
      ENVOY_RUN_DIR=$PWD/check ENVOY_HTTP_PORT=8000 ENVOY_REALTIME_TENANT_HOST=realtime-dev.realtime
    export DASHBOARD_BASIC_AUTH="supabase:{SHA}$(printf x | openssl sha1 -binary | openssl base64)"
    ${lib.concatMapStringsSep "\n" (s: "export ENVOY_${s}_HOST=${lib.toLower s} ENVOY_${s}_PORT=1")
      [ "AUTH" "REST" "REALTIME" "STORAGE" "FUNCTIONS" "META" "STUDIO" ]}
    ${renderConfig} $out check
    if grep -nF '$'"{" check/*.yaml; then
      echo "unrendered placeholder" >&2; exit 1
    fi
    # Static resources take no `@type` key.
    yq -n '{"static_resources": {
        "listeners": (load("check/lds.yaml").resources | map(del(."@type"))),
        "clusters": (load("check/cds.yaml").resources | map(del(."@type")))}}
      * (load("check/envoy.yaml") | del(.dynamic_resources))' > check/static.yaml
    HOME=$PWD envoy --mode validate -c check/static.yaml
  '';
in
firestream.mkContainerModule {
  name = "envoy";
  inherit version paths envVars envVarsWithSecrets;
  inherit imageName imageTag exposedPorts health;

  validateFn = ''
    for v in ANON_KEY SERVICE_ROLE_KEY DASHBOARD_PASSWORD; do
      [[ -n "''${!v:-}" ]] || print_validation_error "$v is required"
    done
  '';

  # Upstream docker/volumes/api/envoy/docker-entrypoint.sh, with the
  # addresses added.
  configFn = ''
    password_hash=$(printf '%s' "$DASHBOARD_PASSWORD" | openssl sha1 -binary | openssl base64)
    export DASHBOARD_BASIC_AUTH="''${DASHBOARD_USERNAME}:{SHA}''${password_hash}"
    mkdir -p "$ENVOY_RUN_DIR"
    ${renderConfig} ${templates} "$ENVOY_RUN_DIR"
    if [[ -n "''${SUPABASE_SECRET_KEY:-}" && -n "''${SUPABASE_PUBLISHABLE_KEY:-}" \
          && -n "''${SERVICE_ROLE_KEY_ASYMMETRIC:-}" && -n "''${ANON_KEY_ASYMMETRIC:-}" ]]; then
      info "Envoy sb_ key translation enabled"
    else
      info "Envoy running in legacy API key mode (sb_ keys disabled)"
    fi
  '';

  runCmd = ''envoy -c "$ENVOY_RUN_DIR/envoy.yaml"'';
  enableStateTracking = false;

  runtimeDirs = {
    run = {
      path = "/opt/firestream/envoy/run";
      type = "tmp";
      persistence = "ephemeral";
      mode = "0755";
      owner = 1001;
      group = 1001;
      description = "Rendered Envoy config (holds the API keys)";
    };
  };

  systemDeps = with pkgs; [ cacert coreutils ];
  runtimeBinDeps = [ envoy ] ++ (with pkgs; [ coreutils bash gnugrep openssl ]);

  user = { name = "envoy"; group = "envoy"; uid = 1001; gid = 1001; };
}
