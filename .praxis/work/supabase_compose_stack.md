---
id: FIRE-028
title: "Supabase compose stack"
type: feature
status: done
priority: medium
tags: [compose, phase-3, supabase]
affects: []
depends-on: [FIRE-016, FIRE-019, FIRE-020, FIRE-021, FIRE-022, FIRE-023, FIRE-024, FIRE-025, FIRE-026, FIRE-027, FIRE-032]
---

# Supabase compose stack

Epic: [[FIRE-009]]

## Goal

Run the complete Supabase stack locally with `nix run .#supabase-up`.

## Scope

- Put the `compose` block in the options of the primary image (the gateway
  or Studio). Refer to the other images as `@key` entries and list them in
  `dependencies`. The schema is in
  `bin/nix/firestream/containers/eval-container.nix` (the compose
  submodule). Follow `src/containers/firestream/airflow/options.nix`.
- Include SeaweedFS as the Storage backend.
- Use the development secrets from [[FIRE-018]].
- Give each service a health check and `dependsOn` in startup order:
  Postgres, then migrations, then Auth, PostgREST, Realtime, Supavisor,
  Storage and postgres-meta, then the gateway and Studio.
- Choose a `hostPortOffset` that no other stack uses.

## Acceptance

- `nix run .#supabase-up` starts every service healthy.
- Studio, the REST API and Auth answer through the gateway port.
- `nix run .#supabase-down` removes the stack.

## Spike findings

The spike [[FIRE-013]] found that the SeaweedFS compose stack has no S3 auth, unlike the chart. Give SeaweedFS an S3 config in the Supabase compose stack, so Storage uses authenticated S3 in both targets.

## Phase 2 facts

From the Phase 2 report. Each image ticket's work log has the details.

| Service | Package | Port | Readiness | Main env |
|---|---|---|---|---|
| db | `supabase-postgres` | 5432 | `pg_isready` | `POSTGRES_PASSWORD` |
| rest | `postgrest` | 3000 (admin 3001 on localhost) | admin `/ready` | `PGRST_DB_URI` (authenticator), `PGRST_JWT_SECRET` = `JWT_JWKS` |
| auth | `supabase-auth` | 9999 | `/health` | `GOTRUE_DB_DATABASE_URL` (supabase_auth_admin), `GOTRUE_JWT_SECRET`, SMTP settings, or `GOTRUE_MAILER_AUTOCONFIRM=true` |
| realtime | `supabase-realtime` | 4000 | `/healthcheck` | `DB_PASSWORD`, `DB_ENC_KEY` = `REALTIME_DB_ENC_KEY`, `API_JWT_SECRET`, `METRICS_JWT_SECRET`, `SECRET_KEY_BASE` |
| supavisor | `supavisor` | 4000, 5432 (session), 6543 (transaction) | `/api/health` | `POSTGRES_PASSWORD`, `SECRET_KEY_BASE`, `VAULT_ENC_KEY`, both JWT secrets. Clients log in as `<role>.firestream`. |
| imgproxy | `imgproxy` | 5001 | `imgproxy health` | none required |
| meta | `postgres-meta` | 8080 | `/health` | `PG_META_DB_*`, `CRYPTO_KEY` = Studio's `PG_META_CRYPTO_KEY` |
| storage | `supabase-storage` | 5000 | `/status` | `DATABASE_URL`, `ANON_KEY`, `SERVICE_KEY`, `AUTH_JWT_SECRET`, S3 settings |
| studio | `supabase-studio` | 3000 | `/api/platform/profile` | `STUDIO_PG_META_URL`, `SUPABASE_URL`, keys, `AUTH_JWT_SECRET` |
| envoy | `supabase-envoy` | 8000 (admin 9901 on localhost) | admin `/ready` | `ANON_KEY`, `SERVICE_ROLE_KEY`, `DASHBOARD_PASSWORD`; the sb_ keys are optional |

Rules:
- Every image also runs healthd on port 9180 (`/readyz`).
- Secrets accept `_FILE`. Take the values from `firestreamLib.supabase.devSecrets`.
- Do not set an empty value for a GoTrue number or boolean, because GoTrue then fails at boot.
- Start order: db and SeaweedFS with its bucket (`supabase-storage`), then rest, auth, realtime, supavisor, meta and imgproxy, then storage and studio, and envoy last.
- Envoy reads its upstream addresses from `ENVOY_<SVC>_HOST` and `ENVOY_<SVC>_PORT`. The defaults are the compose service names.
- Studio needs a writable `/firestream/studio/data`. Use `STUDIO_BIND_ADDRESS`; do not set `HOSTNAME`.
- The images change again in [[FIRE-032]] (IFD removal). Start after it.

## SeaweedFS run command fault

Found in the [[FIRE-032]] review. The stock SeaweedFS image does not start under `docker run` or compose. `src/containers/firestream/seaweedfs/module.nix` sets `runCmd` to `exec weed server …`. The base factory already prefixes `exec`, so the shell runs `exec exec weed …` and exits with code 127. The chart is not affected, because it sets its own `command:`.

Fix it in this ticket: remove the inner `exec`. Then prove that the plain image starts under `docker run`, and that `seaweedfs-render-fidelity` and the SeaweedFS e2e still pass.

Other notes from [[FIRE-032]]:
- When postgres-meta, Storage or Studio is bumped, update the YAML lockfile, run `bin/nix/firestream/node/pnpm-lock-json.sh`, and `git add` both files.
- The `profile.nix` header says "13 granular checks". That count is out of date; fix it in [[FIRE-030]].

## Work log

### 2026-10-22

Compose stack done. `nix run .#supabase-up` starts the full stack; `nix run .#supabase-down` removes it.

Files:
- `src/containers/firestream/supabase-envoy/compose.nix` (new): the stack, declared on the gateway image. It takes `devSecrets` from `firestreamLib.supabase.devSecrets`. Project `firestream-supabase`, `hostPortOffset` 42000.
- `nix/flake-modules/containers/supabase-envoy.nix`: loads compose.nix next to options.nix.
- `nix/flake-modules/supabase.nix`: `packages.supabase-compose`, `apps.supabase-up`, `apps.supabase-down` point at the `supabase-envoy` outputs.
- `bin/nix/firestream/containers/eval-container.nix` and `nix/flake-modules/compose.nix`: two new optional service fields, `command` and `dependsOnCompleted` (condition `service_completed_successfully`). Both default to empty and are then omitted.
- `src/containers/firestream/seaweedfs/module.nix`: the run command fault is fixed (no inner `exec`).

Services: db, seaweedfs, seaweedfs-buckets (one-shot), rest, auth, realtime, supavisor, meta, imgproxy, storage, studio, api-gw. Host ports: gateway 50000, gateway healthd 51180, Postgres 47432, Supavisor transaction port 48543. Each service has a health check on healthd `/readyz`; SeaweedFS has a TCP check on 8333.

Verification:
- `nix run .#supabase-up`: exit 0 in 134 s; all 11 services healthy; seaweedfs-buckets exited 0.
- Through the gateway: `/auth/v1/health` 200; `/rest/v1/<table>` with the anon key 200 with rows; `/` without login 401; Studio `/project/default` with the dashboard login 200; Storage bucket create, upload and download through `/storage/v1` 200 and the same bytes; `select 42` through Supavisor 6543 as `postgres.firestream` returns 42.
- Anonymous S3 on SeaweedFS returns 403, so S3 auth is on.
- `nix run .#supabase-down`: no containers and no network remain. `-- -v` also removes the three volumes.
- Shared-code proof: I built all 30 `*-compose` files before and after the change and compared them byte for byte. Only `supabase-envoy` changed. In one tree, the Envoy image drvPath is the same with the compose block and with `compose = mkForce {}`.
- SeaweedFS fix: the plain image runs under `docker run` and opens S3 on 8333. `make test-e2e-k8s-seaweedfs` passes (13 passed).

Deviations:
- The compose block is in `supabase-envoy/compose.nix`, not in options.nix, because it needs `devSecrets` from firestreamLib. This is the same pattern as `postgresql/supabase.nix`.
- compose.nix names apps after the image key, so the stack is `supabase-envoy-*`; `supabase-*` are aliases.
- No `seaweedfs-render-fidelity` check exists in the flake. The SeaweedFS chart sets its own `command:`, so the run command change does not reach it; the k8s e2e above covers the chart.
- The health check uses bash built-ins only, because the imgproxy image has no grep or head.

Open items: the Envoy RBAC returns 403 for the anon key on `/rest/v1/` (the OpenAPI root). That is the upstream route config, not a fault.
