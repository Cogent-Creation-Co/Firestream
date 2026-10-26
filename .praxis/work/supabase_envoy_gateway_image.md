---
id: FIRE-021
title: "Envoy gateway image and Supabase routes"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: [FIRE-018]
---

# Envoy gateway image and Supabase routes

Epic: [[FIRE-009]]

## Goal

Package Envoy as the Supabase API gateway, with the upstream route and key
configuration.

## Scope

- Use `pkgs.envoy` (1.36.5 in our pin). Upstream pins v1.39.1. Decide if
  1.36.5 is acceptable for the upstream config.
- Port the upstream Envoy config from `supabase/supabase`
  (`docker/volumes/api/`) into a Nix-rendered config:
  - Routes: `/auth/v1`, `/rest/v1`, `/graphql/v1`, `/realtime/v1`,
    `/storage/v1`, `/pg/` and Studio.
  - The anon and service key checks.
  - The translation of `sb_publishable_*` and `sb_secret_*` keys to
    internal JWTs.
- Make the upstream service addresses options, so compose and the chart can
  set them.
- Add the image files, the flake-module, the `flake.nix` import and the CI
  profile entry.

## Acceptance

- The image builds and the config passes `envoy --mode validate`.
- With the other services up, a request with the anon key reaches PostgREST
  through the gateway.

## Work log

### 2026-10-22

Done. The Envoy gateway image builds, validates the upstream config, and routes to PostgREST, Auth and Realtime.

Files:
- `src/containers/firestream/supabase-envoy/{options.nix,module.nix,render-templates.py}`: container `envoy`, image `firestream-supabase-envoy:1.36.2`.
- `nix/flake-modules/containers/supabase-envoy.nix`: `packages.supabase-envoy`, `firestreamContainers.supabase-envoy`, `firestreamImages.supabase-envoy`. Also the `flake.nix` import and the `containerPackages` entry.

Config method: the source is the upstream `docker/volumes/api/envoy/{envoy,cds,lds.template}.yaml` from `firestream.supabase.sources.dockerVolumes`, so an upstream sync is a pin bump. At build time `render-templates.py` replaces the hard-coded addresses with `ENVOY_<SVC>_HOST`/`ENVOY_<SVC>_PORT` (AUTH, REST, REALTIME, STORAGE, FUNCTIONS, META, STUDIO), `ENVOY_HTTP_PORT` and `ENVOY_REALTIME_TENANT_HOST`. Each replacement must match the expected count, or the build fails. The build then renders the templates with test values, merges LDS and CDS into `static_resources`, and runs `envoy --mode validate`: 7 clusters, 1 listener, `OK`. At boot the entrypoint does what upstream `docker-entrypoint.sh` does: it hashes `DASHBOARD_PASSWORD`, fills the templates with `envsubst` (an explicit variable list, because the Lua code holds `$`), and writes them to `ENVOY_RUN_DIR`.

Version: the pin's Envoy 1.36.2 (upstream runs v1.39.1). The binary cache does not have it, so Nix builds it from source with Bazel (about 50 minutes on 32 cores). The pin's Bazel deps hash no longer matches, so the module overrides `depsHash` (`sha256-J1TL/F9n...`). WARNING in the module: the Bazel fetch is not reproducible over time; update the hash when it fails again.

Verification (docker, with the PostgREST, Auth and Realtime images up):
- `/rest/v1/todos` with `apikey: ANON_KEY`: 200 with rows. No key: 401. A bad key: 401.
- sb_ key translation on (all four keys set): `apikey: sb_publishable_*` and `apikey: sb_secret_*` on `/rest/v1/todos`: 200. In legacy mode (no sb_ keys) the publishable key gets 401, and the log says so.
- `/auth/v1/health`: 200 (GoTrue v2.197.0). `/auth/v1/signup` with the publishable key: 200.
- Realtime websocket through `/realtime/v1/websocket`: joined, and an insert arrived as a `postgres_changes` event.
- `/graphql/v1`: 200 from PostgREST. The body says pg_graphql is not enabled. That is upstream: migration `20260421000000_pg_graphql-off-by-default.sql`.
- `/` (Studio) without basic auth: 401. With basic auth: 503 (Studio was not running).
- healthd `/readyz` (Envoy admin `/ready`): 200.

Deviations and open items:
- The readiness probe sends HTTP/1.1, because the Envoy admin answers 426 to HTTP/1.0.
- `/realtime/v1/api/tenants*` is blocked by the upstream config (403 RBAC), also with the service key. That is upstream.
- A `?apikey=sb_publishable_*` query parameter on `/rest/v1/` gives 400 from PostgREST: the upstream Lua translates the value to a JWT but leaves the parameter, and PostgREST reads it as a filter. This is the upstream config unchanged.
- `/pg/` (postgres-meta), `/storage/v1/` and Studio were not tested here; their images are in FIRE-023, FIRE-024 and FIRE-025.

For Phase 3: port 8000; admin 9901 on 127.0.0.1 only. Required env: `ANON_KEY`, `SERVICE_ROLE_KEY`, `DASHBOARD_PASSWORD`; optional `SUPABASE_PUBLISHABLE_KEY`, `SUPABASE_SECRET_KEY`, `ANON_KEY_ASYMMETRIC`, `SERVICE_ROLE_KEY_ASYMMETRIC`, `SUPABASE_PUBLIC_URL`, `DASHBOARD_USERNAME`. The upstream defaults are the compose names (auth:9999, rest:3000, realtime-dev.supabase-realtime:4000, storage:5000, functions:9000, meta:8080, studio:3000). In Kubernetes set `ENVOY_REALTIME_HOST` to the Realtime Service and keep `ENVOY_REALTIME_TENANT_HOST` starting with `realtime-dev.`. `ENVOY_RUN_DIR` holds the keys: mount an emptyDir there. The compose healthcheck can be the same TCP check as upstream, or `GET :9180/readyz`.
