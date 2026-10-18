#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Full local dev loop for the Firestream Cloudflare-edge example on k3s / k3d.
#
# Three charts, one namespace, deployed in a deliberate order (see DEPLOY ORDER
# below). As in the other *-k3s examples there is no registry: the Nix-built
# `firestream-*` images only exist on your machine, so we build them and
# SIDE-LOAD them into the cluster's containerd before deploying.
#
# cloudflared is the exception -- it runs Cloudflare's own upstream image, so
# there is nothing to side-load and the kubelet pulls it from docker.io.
#
# Prereqs: nix, docker, kubectl, helm, and EITHER host k3s (membership in the
# `k3s` group -- no sudo) OR a k3d cluster (set K3D_CLUSTER=<name>).
#
# In-repo testing before this lands on `main`: the flake pins github, which
# won't have these charts until merged. Build against the local checkout with
#   NIX_OVERRIDE="--override-input firestream path:../.." bash scripts/deploy-local.sh
# ---------------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck disable=SC2086  # NIX_OVERRIDE is intentionally word-split
nixx() { local sub="$1"; shift; nix "$sub" ${NIX_OVERRIDE:-} "$@"; }
nixval() { nixx eval --raw --impure --expr "(import ./config.nix).$1"; }

NS="$(nixval namespace)"
HOSTNAME_="$(nixval hostname)"
ODOO_PW="$(nixval odooPassword)"
TUNNEL_SECRET="$(nixval tunnelSecretName)"

# --- Images -----------------------------------------------------------------
load_image() {
  local pkg="$1"
  nixx build ".#$pkg" --out-link "result-$pkg" >&2
  docker load < "result-$pkg" | sed -n 's/^Loaded image: //p' | head -n1
}

import_image() {
  local ref="$1"
  if [ -n "${K3D_CLUSTER:-}" ]; then
    echo "==> Importing $ref into k3d cluster '$K3D_CLUSTER'" >&2
    k3d image import "$ref" -c "$K3D_CLUSTER"
  else
    echo "==> Importing $ref into host k3s containerd" >&2
    if command -v k3s >/dev/null 2>&1; then
      docker save "$ref" | k3s ctr -n k8s.io images import -
    else
      docker save "$ref" | ctr -a /run/k3s/containerd/containerd.sock -n k8s.io images import -
    fi
  fi
}

echo "==> Building + loading images" >&2
for pkg in odoo-image nginx-image postgresql-image; do
  import_image "$(load_image "$pkg")"
done

# --- The tunnel token -------------------------------------------------------
# THE ONE THING THAT CANNOT BE REAL LOCALLY.
#
# A tunnel token is issued by Cloudflare when a tunnel is created; it both
# identifies the tunnel and authorises connectors to register against it. With
# no Cloudflare account there is no tunnel and no token, so we write a
# syntactically valid FAKE one: base64 of {"a":<account>,"t":<tunnel>,"s":<secret>},
# which is the shape cloudflared parses before it ever reaches the network.
#
# The connector will therefore START, fail to register with the Cloudflare edge,
# and never report Ready. That is the honest outcome and it is deliberate: the
# alternative -- stubbing the connector out -- would mean the example never
# exercises the chart at all. Everything BEHIND the connector (nginx routing,
# the Odoo 8069/8072 split) is fully testable; see `make smoke`.
#
# The namespace is created here rather than by helm because the Secret has to
# exist before cloudflared's pod starts (its secretKeyRef is non-optional).
fake_tunnel_token() {
  local secret
  secret="$(printf 'firestream-example-fake-tunnel-secret' | base64 -w0)"
  printf '{"a":"%s","t":"%s","s":"%s"}' \
    "0123456789abcdef0123456789abcdef" \
    "11111111-2222-3333-4444-555555555555" \
    "$secret" | base64 -w0
}

echo "==> Ensuring namespace '$NS' and the tunnel-token Secret" >&2
kubectl create namespace "$NS" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl -n "$NS" create secret generic "$TUNNEL_SECRET" \
  --from-literal=tunnel-token="$(fake_tunnel_token)" \
  --dry-run=client -o yaml | kubectl apply -f - >/dev/null

# --- DEPLOY ORDER -----------------------------------------------------------
# 1. odoo         the backend, first, so its Service exists before anything
#                 tries to proxy to it. With config.nix's `clusterDns` empty,
#                 nginx resolves upstream names at CONFIG LOAD and refuses to
#                 start if the Service is missing -- so this ordering is load
#                 bearing. Set `clusterDns` to lift that constraint.
# 2. nginx        the proxy, once there is something to proxy to.
# 3. cloudflared  last. Its readiness depends only on the Cloudflare edge, never
#                 on an in-cluster Service, and the edge resolves the tunnel's
#                 ingress rules at REQUEST time -- so it would start happily
#                 with nothing behind it. Deploying it last just means the
#                 tunnel goes live once there is something to reach, rather than
#                 briefly answering 502.
echo "==> Deploying odoo" >&2
nixx build '.#odoo-chart' --out-link result-odoo-chart
./result-odoo-chart/bin/deploy --namespace "$NS"

echo "==> Deploying nginx" >&2
nixx build '.#nginx-chart' --out-link result-nginx-chart
./result-nginx-chart/bin/deploy --namespace "$NS"

echo "==> Deploying cloudflared" >&2
nixx build '.#cloudflared-chart' --out-link result-cloudflared-chart
./result-cloudflared-chart/bin/deploy --namespace "$NS"

cat <<EOF

==> Done. Watch the rollout:
      kubectl -n $NS get pods -w

    EXPECT cloudflared to stay 0/1 Running and never go Ready. It is trying to
    register a fake tunnel with the Cloudflare edge. Everything else should
    reach Ready.

    Prove the proxy works end to end (nginx -> odoo, both ports):
      make smoke

    Or by hand, from inside the cluster:
      kubectl -n $NS run curl --rm -it --restart=Never --image=curlimages/curl:8.11.1 -- \\
        curl -s -o /dev/null -w '%{http_code}\\n' -H 'Host: $HOSTNAME_' http://nginx/

    Odoo login: user@example.com / $ODOO_PW
EOF
