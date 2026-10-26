#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Start the Firestream Odoo stack with Docker Compose.
#
# We build OUR custom image (vendored addons from ../config.nix) and load it
# into Docker, then bring up the Firestream-generated compose file. We do NOT
# use `nix run firestream#odoo-up`, because that rebuilds the *stock* image and
# would clobber our custom load — the generated compose references the image by
# name (firestream-odoo:18.0), so loading our build first is all it takes.
#
# Run from examples/odoo-docker-compose/ (or anywhere — it cd's to its own dir).
# Prereqs: nix, docker (with the compose plugin).
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

PROJECT="firestream-odoo"

# Build each image (a buildLayeredImage tarball) and load it into Docker. The
# package is a tarball, not a runnable app, so we use `nix build` + `docker
# load` (not `nix run -- --load`). The generated compose references the images
# by name, so loading our custom build first is all that's needed.
load_image() {
  local pkg="$1"
  nixx build ".#$pkg" --out-link "result-$pkg"
  docker load < "result-$pkg"
}

echo "==> Building + loading the custom Odoo image and PostgreSQL image"
load_image odoo-image
load_image postgresql-image

echo "==> Resolving the generated docker-compose file"
COMPOSE="$(nixx build '.#odoo-compose' --no-link --print-out-paths)/docker-compose.yml"
echo "    $COMPOSE"

echo "==> docker compose up -d"
docker compose -f "$COMPOSE" -p "$PROJECT" up -d "$@"

cat <<EOF

==> Up. Check status:
      docker compose -p $PROJECT ps

    Open Odoo at http://127.0.0.1:8069
      login: admin
      password: admin
    (baked container defaults; the DB user is odoo/odoo on host port 18432)

    Stop with: make down     (add -v / 'make clean' to drop the data volume)
EOF
