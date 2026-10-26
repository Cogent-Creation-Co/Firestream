#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Start the Firestream Next.js stack with Docker Compose.
#
# Builds OUR custom image (app baked in from ../app) + the postgres image, loads
# them into Docker, then brings up the Firestream-generated compose file. The
# generated compose references the images by name (firestream-nextjs:1.0.0 and
# firestream-postgresql:17), so loading our build first is all it takes.
#
# Run from examples/nextjs-docker-compose/. Prereqs: nix, docker (+ compose).
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

PROJECT="firestream-nextjs"

load_image() {
  local pkg="$1"
  nixx build ".#$pkg" --out-link "result-$pkg"
  docker load < "result-$pkg"
}

echo "==> Building + loading the custom Next.js image and PostgreSQL"
load_image nextjs-image
load_image postgresql-image

echo "==> Resolving the generated docker-compose file"
COMPOSE="$(nixx build '.#nextjs-compose' --no-link --print-out-paths)/docker-compose.yml"
echo "    $COMPOSE"

echo "==> docker compose up -d"
docker compose -f "$COMPOSE" -p "$PROJECT" up -d "$@"

cat <<EOF

==> Up. Check status:
      docker compose -p $PROJECT ps

    Open the app at http://127.0.0.1:3000
      (the homepage runs a live SELECT against firestream-postgresql)
    Health:  curl http://127.0.0.1:3000/api/health
    Postgres is published on 127.0.0.1:20432.

    Stop with: make down     (use 'make clean' to also drop the data volume)
EOF
