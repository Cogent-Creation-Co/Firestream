# Supabase shared tooling flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - packages.supabase-keygen / apps.supabase-keygen
#       (prints a production key set; see bin/nix/firestream/supabase/keygen.py)
#   - checks.firestream-supabase-dev-secrets
#       (the published dev set in firestream.supabase.devSecrets is consistent)
#   - packages.supabase-compose / apps.supabase-up / apps.supabase-down
#       (the compose stack declared on the gateway image; see
#       src/containers/firestream/supabase-envoy/compose.nix)
{ ... }: {
  perSystem = { pkgs, lib, config, firestreamLib, ... }:
    let
      inherit (firestreamLib.supabase) keygen devSecrets;

      devEnv = pkgs.writeText "supabase-dev-secrets.env"
        (lib.concatStrings (lib.mapAttrsToList (k: v: "${k}=${v}\n") devSecrets));

      pyjwt = pkgs.python3.withPackages (ps: [ ps.pyjwt ps.cryptography ]);
    in
    {
      packages.supabase-keygen = keygen;

      # NOTE: compose.nix names a stack after the image that declares it. The
      # stack is Supabase, not its gateway, so the e2e harness and users
      # address it as `supabase`.
      packages.supabase-compose = config.packages.supabase-envoy-compose;
      apps.supabase-up = config.apps.supabase-envoy-up;
      apps.supabase-down = config.apps.supabase-envoy-down;

      apps.supabase-keygen = {
        type = "app";
        program = lib.getExe keygen;
        meta.description = "Print a consistent key set for a self-hosted Supabase stack";
      };

      # NOTE: every Supabase service trusts tokens signed with JWT_SECRET. If
      # one dev value is regenerated alone, the others stop verifying and the
      # stack fails at its first authenticated request. PyJWT verifies the
      # tokens a second time, so a defect shared by keygen's signer and its
      # verifier cannot pass.
      checks.firestream-supabase-dev-secrets =
        pkgs.runCommand "firestream-supabase-dev-secrets" { nativeBuildInputs = [ keygen pyjwt ]; } ''
          set -euo pipefail
          supabase-keygen --verify ${devEnv}
          python - ${devEnv} <<'PY'
          import json, sys, jwt
          v = dict(line.rstrip("\n").split("=", 1) for line in open(sys.argv[1]))
          for name, role in (("ANON_KEY", "anon"), ("SERVICE_ROLE_KEY", "service_role")):
              assert jwt.decode(v[name], v["JWT_SECRET"], algorithms=["HS256"])["role"] == role, name
          jwks = jwt.PyJWKSet.from_dict(json.loads(v["JWT_JWKS"]))
          for name, role in (("ANON_KEY_ASYMMETRIC", "anon"), ("SERVICE_ROLE_KEY_ASYMMETRIC", "service_role")):
              kid = jwt.get_unverified_header(v[name])["kid"]
              assert jwt.decode(v[name], jwks[kid].key, algorithms=["ES256"])["role"] == role, name
          PY
          supabase-keygen --output env > generated.env
          supabase-keygen --verify generated.env
          echo "PASS: dev secrets verify with keygen and PyJWT; a fresh set verifies"
          touch "$out"
        '';
    };
}
