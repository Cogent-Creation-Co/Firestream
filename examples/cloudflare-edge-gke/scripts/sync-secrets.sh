#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Materialise the Odoo Kubernetes Secrets from GCP Secret Manager. Idempotent.
#
# NOT the tunnel token. That one is written by Pulumi, because Cloudflare mints
# it as an output of the tunnel resource -- it never exists anywhere this script
# could read it from. See ../pulumi/__main__.py.
#
# The namespace is NOT created here either: Pulumi owns it (which is why the
# chart overrides set `_meta.createNamespace = false`).
#
# Usage: sync-secrets.sh <namespace>
# ---------------------------------------------------------------------------
set -euo pipefail

NS="${1:-edge}"

access() { gcloud secrets versions access latest --secret="$1"; }

echo "Syncing odoo-credentials into '$NS'..." >&2
kubectl create secret generic odoo-credentials \
  --namespace "$NS" \
  --from-literal=odoo-password="$(access odoo-password)" \
  --dry-run=client -o yaml | kubectl apply -f -

echo "Syncing odoo-db-credentials into '$NS'..." >&2
kubectl create secret generic odoo-db-credentials \
  --namespace "$NS" \
  --from-literal=password="$(access odoo-db-password)" \
  --from-literal=postgres-password="$(access odoo-db-postgres-password)" \
  --dry-run=client -o yaml | kubectl apply -f -

echo "Secrets synced into namespace '$NS'." >&2
