# Regression guard: Bitnami `POSTGRES_* -> POSTGRESQL_*` alias bridge.
# Copyright Firestream. MIT License.
#
# The Bitnami postgresql Helm chart injects DB credentials under docker-style
# env names (`POSTGRES_USER`, `POSTGRES_DATABASE`, `POSTGRES_PASSWORD[_FILE]`,
# …), but every init/helper script in the firestream-postgresql container reads
# the `POSTGRESQL_*` prefix. The container bridges the two by giving each
# `POSTGRESQL_*` env default a `${POSTGRES_*:-<default>}` fallback (see
# src/containers/firestream/postgresql/options.nix). Without that bridge the
# custom role/database are never created and chart consumers (odoo, airflow,
# superset, jupyterhub, standalone postgresql) crash-loop with
# `role "<user>" does not exist`.
#
# This test imports the REAL container options (the source eval-container.nix
# consumes on the image-build path), unwraps the per-leaf `lib.mkDefault`s, and
# drives the generated env-defaults + secrets file-loader exactly as the
# container entrypoint does. It tracks options.nix rather than a copy, so
# dropping an alias fails the build.

{ pkgs, firestream }:

let
  lib = pkgs.lib;
  defaults = import ../env/defaults.nix { inherit pkgs lib; };
  fileLoader = import ../env/file-loader.nix { inherit pkgs lib; };

  # NOTE: the test reads only `config.postgresql.env*`, which never touch the
  # module's own `config`. A throw keeps it that way: an env default that
  # starts to depend on other options fails here with a named error.
  pgOptions = import ../../../../src/containers/firestream/postgresql/options.nix {
    inherit lib pkgs;
    config = throw "test-postgresql-env-aliases: the env defaults must not read other options";
  };
  unwrap = v: v.content or v; # strip lib.mkDefault override wrappers
  pgEnv = lib.mapAttrs (_: unwrap) pgOptions.config.postgresql.env;
  pgSecrets = unwrap pgOptions.config.postgresql.envSecrets;

  envDefaults = defaults.mkEnvDefaults {
    appName = "postgresql";
    envVars = pgEnv;
  };
  loadEnvFiles = fileLoader.mkFileLoader {
    appName = "postgresql";
    secretVars = pgSecrets;
  };
in
pkgs.runCommand "test-postgresql-env-aliases" { } ''
  set -euo pipefail
  fail() { echo "FAIL: $1" >&2; exit 1; }

  echo "=== PostgreSQL POSTGRES_* -> POSTGRESQL_* alias bridge ==="

  # (1) chart injects docker-style POSTGRES_USER / POSTGRES_DATABASE
  (
    export POSTGRES_USER="bn_test"
    export POSTGRES_DATABASE="testdb"
    source ${envDefaults}/lib/postgresql-env-defaults.sh
    [[ "$POSTGRESQL_USERNAME" == "bn_test" ]] || fail "USERNAME alias: got '$POSTGRESQL_USERNAME'"
    [[ "$POSTGRESQL_DATABASE" == "testdb" ]] || fail "DATABASE alias: got '$POSTGRESQL_DATABASE'"
    echo "ok: POSTGRES_USER/DATABASE -> POSTGRESQL_USERNAME/DATABASE"
  )

  # (2) chart mounts the password as a file (POSTGRES_PASSWORD_FILE); env-defaults
  #     must alias it to POSTGRESQL_PASSWORD_FILE *before* the file-loader runs,
  #     so the loader populates POSTGRESQL_PASSWORD.
  (
    tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
    printf '%s' "filesecret" > "$tmp"
    export POSTGRES_PASSWORD_FILE="$tmp"
    source ${envDefaults}/lib/postgresql-env-defaults.sh
    source ${loadEnvFiles}/lib/postgresql-load-env-files.sh
    [[ "$POSTGRESQL_PASSWORD" == "filesecret" ]] || fail "PASSWORD_FILE alias: got '$POSTGRESQL_PASSWORD'"
    echo "ok: POSTGRES_PASSWORD_FILE -> POSTGRESQL_PASSWORD"
  )

  # (3) nothing injected -> backward-compatible defaults (postgres / empty)
  (
    source ${envDefaults}/lib/postgresql-env-defaults.sh
    [[ "$POSTGRESQL_USERNAME" == "postgres" ]] || fail "default USERNAME: got '$POSTGRESQL_USERNAME'"
    [[ -z "$POSTGRESQL_DATABASE" ]] || fail "default DATABASE not empty: '$POSTGRESQL_DATABASE'"
    echo "ok: defaults preserved when nothing injected"
  )

  echo "=== PostgreSQL alias bridge OK ==="
  touch $out
''
