#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Start the Firestream Airflow stack with Docker Compose.
#
# Builds OUR custom image (DAG baked in from ../dags) + the postgres/redis
# images, loads them into Docker, then brings up the Firestream-generated
# compose file. We do NOT use `nix run firestream#airflow-up` because that
# rebuilds the *stock* image and would clobber our custom load — the generated
# compose references the image by name (firestream-airflow:3.0.3), so loading
# our build first is all it takes.
#
# Run from examples/airflow-docker-compose/. Prereqs: nix, docker (+ compose).
#
# In-repo testing before this lands on `main` (the flake pins github):
#   NIX_OVERRIDE="--override-input firestream path:../.." bash scripts/up.sh
# ---------------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")/.."

# NIX_OVERRIDE (e.g. "--override-input firestream path:../..") is a SUBCOMMAND
# flag, so it must go AFTER the subcommand: `nix build --override-input ...`.
# shellcheck disable=SC2086  # NIX_OVERRIDE is intentionally word-split
nixx() { local sub="$1"; shift; nix "$sub" ${NIX_OVERRIDE:-} "$@"; }

PROJECT="firestream-airflow"

# Build each image (a buildLayeredImage tarball) and load it into Docker.
# (Packages are tarballs, not runnable apps, so `nix build` + `docker load`.)
load_image() {
  local pkg="$1"
  nixx build ".#$pkg" --out-link "result-$pkg"
  docker load < "result-$pkg"
}

echo "==> Building + loading the custom Airflow image, PostgreSQL, and Redis"
load_image airflow-image
load_image postgresql-image
load_image redis-image

echo "==> Resolving the generated docker-compose file"
COMPOSE="$(nixx build '.#airflow-compose' --no-link --print-out-paths)/docker-compose.yml"
echo "    $COMPOSE"

echo "==> docker compose up -d"
docker compose -f "$COMPOSE" -p "$PROJECT" up -d "$@"

cat <<EOF

==> Up. Check status:
      docker compose -p $PROJECT ps

    Open the Airflow web UI at http://127.0.0.1:11080
      login: admin
      password: admin
    (baked container defaults; postgres on 11432, redis on 11379)

    The baked DAG appears as 'hello_firestream' in the DAG list.

    Stop with: make down     (use 'make clean' to also drop the data volumes)
EOF
