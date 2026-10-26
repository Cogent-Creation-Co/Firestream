---
id: FIRE-029
title: "Supabase Helm chart and firestreamStacks.supabase"
type: feature
status: done
priority: medium
tags: [chart, phase-3, supabase]
affects: []
depends-on: [FIRE-016, FIRE-019, FIRE-020, FIRE-021, FIRE-022, FIRE-023, FIRE-024, FIRE-025, FIRE-026, FIRE-027, FIRE-032]
---

# Supabase Helm chart and firestreamStacks.supabase

Epic: [[FIRE-009]]

## Goal

Deploy the complete Supabase stack to Kubernetes with
`firestream helm deploy-stack supabase`.

## Scope

- Fork `supabase-community/supabase-kubernetes` (Apache-2.0) into
  `src/charts/firestream/supabase/` as a non-Bitnami chart, like
  `seaweedfs`. Keep the upstream license and note the forked commit.
  Replace Kong with Envoy, and remove the components that are out of scope.
- Add `src/charts/firestream/supabase/nix/` with typed options that mirror
  the `values.yaml` hierarchy.
- Add `nix/flake-modules/charts/supabase.nix`. Give each image one
  `_meta.containerRefs` slot. Follow `nix/flake-modules/charts/seaweedfs.nix`
  and `nextjs.nix`.
- Read the secrets from an existing Secret, with the development defaults
  from [[FIRE-018]].
- Add `checks.supabase-render-fidelity` in
  `nix/flake-modules/charts/checks.nix`.
- Add `firestreamStacks.supabase = [ seaweedfs supabase ]` in
  `nix/flake-modules/charts/aggregate.nix`. Do not add Supabase to `dev`.
- Add `supabase` to `chartNames` in the CI profile.

## Acceptance

- `packages.supabase-chart` and `packages.supabase-base-chart` build, and
  the offline `helm template` gate passes.
- `index.json` in the charts bundle lists the chart and the `supabase`
  stack.
- `firestream helm deploy-stack supabase` comes up healthy on k3d.

## Phase 2 facts

The ports, readiness commands, env and start order are in [[FIRE-028]] "Phase 2 facts". The Kubernetes-specific points:

- Envoy:
  - Set `ENVOY_REALTIME_HOST` to the Realtime Service.
  - Keep `ENVOY_REALTIME_TENANT_HOST` starting with `realtime-dev.`, because Realtime reads the tenant from the Host header.
  - Mount an emptyDir at `ENVOY_RUN_DIR`, because the rendered files hold the API keys.
- Studio: mount a writable volume at `/firestream/studio/data`.
- The BEAM images (realtime, supavisor) need writable mounts at `RELEASE_TMP` and `/tmp` when the root filesystem is read-only.
- Supavisor needs `hostname` in the image; it is there. It names its node with `hostname -f`.
- Database: `postgres` is not a superuser after the migrations. Run superuser jobs, such as a backup CronJob, as `supabase_admin`. The pgsodium root key is at `$POSTGRESQL_VOLUME_DIR/pgsodium_root.key`, so that path must be on the PVC; or supply `POSTGRESQL_PGSODIUM_ROOT_KEY` from a Secret.
- Production secrets: `nix run .#supabase-keygen -- --output k8s-secret --name <name>`.

## Work log

### 2026-10-23

Chart and stack done. `firestream helm deploy-stack supabase` deploys SeaweedFS and Supabase on k3d, and every pod turns Ready.

Files:
- `src/charts/firestream/supabase/` (new): a non-Bitnami fork of supabase-community/supabase-kubernetes at commit 8d4b5cda901a3f28535af5c79c5ed95e884e48d9 (chart 0.8.0, Apache-2.0). LICENSE is kept; Chart.yaml records the commit in `firestream.io/forked-*` annotations. Version `0.8.0-firestream.1`.
  - Envoy replaces Kong (`templates/envoy/`, with an in-memory emptyDir at `deployment.envoy.runDir` = `ENVOY_RUN_DIR`, and `ENVOY_<SVC>_HOST/PORT` set to the release Services).
  - Supavisor is new (`templates/supavisor/`, ports 4000, 5432, 6543).
  - Removed: analytics, functions, vector, minio, kong, the test hooks, the upstream initdb SQL, `docker/`, `scripts/`, `ci/`, README.
  - Secrets: one key Secret in the `supabase-keygen --output k8s-secret` layout (`secret.supabase.secretRef` or `secret.supabase.values`). `secret.s3` and `secret.smtp` keep the upstream secretRef shape. Optional keys (asymmetric keys, sb_ keys, DASHBOARD_USERNAME) use `optional: true`.
  - db: the Firestream image runs the migrations; the PVC is at `/firestream/postgresql`, so `pgsodium_root.key` is on it. `migration` SQL mounts at `/docker-entrypoint-initdb.d`.
  - Storage: an `init-bucket` init container creates `GLOBAL_S3_BUCKET` with the Storage image's own AWS SDK. Default endpoint: the SeaweedFS chart Service.
  - Realtime and Supavisor: emptyDirs at `RELEASE_TMP` (`/opt/firestream/<app>/tmp`) and `/tmp`. Studio: a volume at `/firestream/studio/data`.
  - Every readiness probe is healthd `/readyz` on 9180. The init-db wait uses the db image's `pg_isready`.
- `src/charts/firestream/supabase/nix/` (new): typed options that mirror the values domains (secret, deployment, image, environment, service, autoscaling, serviceAccount, persistence, ingress, networkPolicy, migration, serviceMonitor).
- `nix/flake-modules/charts/supabase.nix` (new): ten `_meta.containerRefs` slots, `componentPath = [ "image" <component> ]`; it writes `devSecrets` into `secret.supabase.values`.
- `nix/flake-modules/charts/checks.nix`: `checks.supabase-render-fidelity`.
- `nix/flake-modules/charts/aggregate.nix`: `firestreamStacks.supabase = [ "seaweedfs" "supabase" ]`. `flake.nix` imports the module.
- `bin/nix/firestream/ci/profile.nix`: `supabase` in `chartNames`.

Verification:
- `nix build .#supabase-chart .#supabase-base-chart`: both build. `helm lint` passes.
- `checks.supabase-render-fidelity`: PASS. The bare and the overlaid renders are byte-identical with no normalisation; it also asserts 9 Deployments, only `firestream-supabase-*` images, no removed component, and no key Secret under `secretRef`. All 12 render-fidelity checks pass.
- `index.json`: the before/after diff adds only `charts.supabase`, `baseCharts.supabase` and `stacks.supabase`.
- `firestream helm deploy-stack supabase --charts-dir <bundle>` on k3d: exit 0 in 42 s; 11 pods Ready. Through a port-forward: Auth health 200, Studio 200 with the dashboard login, Storage bucket, upload and download 200 with the same bytes.
- `make test-e2e-k8s-supabase`: PASS (see FIRE-030).

Deviations:
- The upstream per-domain secrets (jwt, apikey, db, dashboard, meta, realtime) became one Secret in the keygen layout.
- External databases (`deployment.db.enabled=false`) are not supported: the Firestream db image runs the migrations.
- Ingress is off by default. Service names keep the upstream `<release>-supabase-<component>` form, so release `supabase` gives `supabase-supabase-envoy`.
- The chart `values.yaml` holds the development key set, as upstream holds its demo keys. The render-fidelity check keeps it equal to `devSecrets`.

Open items: none for the chart. The bundle `migration` value has no cluster test.
