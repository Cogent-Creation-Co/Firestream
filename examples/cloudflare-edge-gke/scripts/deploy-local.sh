#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Build + push the firestream-* images, then deploy all three charts to GKE.
#
# Assumes `make pulumi-up` has already run: the cluster, the Artifact Registry
# repo, the Secret Manager secrets, the Cloudflare tunnel, the namespace and the
# tunnel-token Secret all come from there.
#
# Reads project-specific values straight out of ../config.nix.
# ---------------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck disable=SC2086
nixx() { local sub="$1"; shift; nix "$sub" ${NIX_OVERRIDE:-} "$@"; }
nixval() { nixx eval --raw --impure --expr "(import ./config.nix).$1"; }

PROJECT="$(nixval projectId)"
REGION="$(nixval region)"
CLUSTER="$(nixval clusterName)"
NS="$(nixval namespace)"
AR_HOST="$(nixval arHost)"
AR_REPO="$(nixval arRepo)"
TAG="$(nixval imageTag)"
HOST="$(nixval hostname)"

echo "==> Configuring docker auth for $AR_HOST" >&2
gcloud auth configure-docker "$AR_HOST" --quiet

push_image() {
  local pkg="$1" name="$2"
  nixx build ".#$pkg" --out-link "result-$pkg" >&2
  local ref; ref="$(docker load < "result-$pkg" | sed -n 's/^Loaded image: //p' | head -n1)"
  local remote="$AR_HOST/$AR_REPO/$name:$TAG"
  docker tag "$ref" "$remote"
  docker push "$remote"
  echo "$remote"
}

echo "==> Building + pushing images" >&2
push_image odoo-image firestream-odoo
push_image nginx-image firestream-nginx
push_image postgresql-image firestream-postgresql

echo "==> Fetching cluster credentials" >&2
gcloud container clusters get-credentials "$CLUSTER" \
  --region "$REGION" --project "$PROJECT"

echo "==> Syncing Odoo secrets (the tunnel token is Pulumi's)" >&2
bash scripts/sync-secrets.sh "$NS"

# DEPLOY ORDER. Only the odoo -> nginx edge is load bearing, and only when
# `clusterDns` is empty in config.nix: without a resolver nginx resolves every
# proxy_pass name at CONFIG LOAD and refuses to start if the Service is missing.
# With `clusterDns` set (the GKE default here) nginx boots regardless and
# answers 502 until Odoo appears, so this ordering is a convenience.
#
# cloudflared genuinely does not care: its readiness depends only on the
# Cloudflare edge, and the edge resolves the tunnel's ingress rules at request
# time. Last just means the tunnel goes live once there is something to reach.
for chart in odoo nginx cloudflared; do
  echo "==> Deploying $chart" >&2
  nixx build ".#$chart-chart" --out-link "result-$chart-chart"
  "./result-$chart-chart/bin/deploy" --namespace "$NS"
done

cat <<MSG

==> Done. Watch the rollout:
      kubectl -n $NS get pods -w

    cloudflared should reach Ready within a minute — that is the connector
    registering with the Cloudflare edge. If it does not, its logs say why:
      kubectl -n $NS logs -l app.kubernetes.io/name=cloudflared

    Then: https://$HOST
MSG
