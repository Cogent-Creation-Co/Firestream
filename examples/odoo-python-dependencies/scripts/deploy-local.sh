#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Full local dev loop for the Firestream Odoo app on k3s / k3d.
#
# Unlike the GKE example there is no registry: the Nix-built `firestream-*`
# images only exist on your machine, so we BUILD them, then SIDE-LOAD them into
# the cluster's containerd image store before `helm upgrade --install`. Without
# this step every pod ImagePullBackOffs trying to reach docker.io.
#
# Reads project-specific values straight out of ../config.nix (single source of
# truth). Run from examples/odoo-python-dependencies/ (or anywhere — it cd's to its own dir).
#
# Targets (auto-detected):
#   - host k3s (default): docker save <ref> | k3s ctr -n k8s.io images import -
#   - k3d:                k3d image import <ref> -c "$K3D_CLUSTER"   (set K3D_CLUSTER)
#
# Prereqs: nix, docker, kubectl, helm, and EITHER host k3s (membership in the
# `k3s` group — no sudo) OR a k3d cluster (set K3D_CLUSTER=<name>).
#
# In-repo testing before this lands on `main`: the flake pins github, which
# won't have the consumer API until merged. Build against the local checkout by
# exporting NIX_OVERRIDE, e.g. from examples/odoo-python-dependencies/:
#   NIX_OVERRIDE="--override-input firestream path:../.." bash scripts/deploy-local.sh
# ---------------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")/.."

# NIX_OVERRIDE (e.g. "--override-input firestream path:../..") is a SUBCOMMAND
# flag, so it must go AFTER the subcommand: `nix build --override-input ...`.
# shellcheck disable=SC2086  # NIX_OVERRIDE is intentionally word-split
nixx() { local sub="$1"; shift; nix "$sub" ${NIX_OVERRIDE:-} "$@"; }
nixval() { nixx eval --raw --impure --expr "(import ./config.nix).$1"; }

NS="$(nixval namespace)"
ODOO_PW="$(nixval odooPassword)"

# Build each image (a buildLayeredImage tarball) and load it into Docker,
# echoing the loaded "repo:tag" ref. (The package is a tarball, not a runnable
# app, so we use `nix build` + `docker load`, not `nix run -- --load`.)
load_image() {
  local pkg="$1"
  nixx build ".#$pkg" --out-link "result-$pkg" >&2
  docker load < "result-$pkg" | sed -n 's/^Loaded image: //p' | head -n1
}

echo "==> Building + loading images into Docker" >&2
ODOO_REF="$(load_image odoo-image)"
PG_REF="$(load_image postgresql-image)"
echo "==> Loaded $ODOO_REF and $PG_REF" >&2

# --- Side-load into the cluster's containerd --------------------------------
import_image() {
  local ref="$1"
  if [ -n "${K3D_CLUSTER:-}" ]; then
    echo "==> Importing $ref into k3d cluster '$K3D_CLUSTER'"
    k3d image import "$ref" -c "$K3D_CLUSTER"
  else
    echo "==> Importing $ref into host k3s containerd"
    # k3s ctr targets /run/k3s/containerd/containerd.sock by default. Fall back
    # to a bare ctr against that socket if the k3s wrapper isn't on PATH.
    if command -v k3s >/dev/null 2>&1; then
      docker save "$ref" | k3s ctr -n k8s.io images import -
    else
      docker save "$ref" | ctr -a /run/k3s/containerd/containerd.sock -n k8s.io images import -
    fi
  fi
}
import_image "$ODOO_REF"
import_image "$PG_REF"

echo "==> Building customized chart bundle"
nixx build '.#odoo-chart' --out-link result

echo "==> Deploying Odoo (helm upgrade --install)"
./result/bin/deploy --namespace "$NS"

cat <<EOF

==> Done. Watch rollout:
      kubectl -n $NS get pods -w

    Once both pods are Running, expose Odoo:
      kubectl -n $NS port-forward svc/odoo 8069:80

    Then open http://127.0.0.1:8069
      login: user@example.com
      password: $ODOO_PW
EOF
