---
id: FIRE-026
title: "Realtime image and tenant seed"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: [FIRE-016, FIRE-017]
---

# Realtime image and tenant seed

Epic: [[FIRE-009]]

## Goal

Build `supabase/realtime` v2.143.3 with Nix and package it with the BEAM
factory. Make the tenant setup automatic.

## Scope

- Build the release with the method from the spike [[FIRE-010]] and wrap it
  with the BEAM factory from [[FIRE-017]].
- Add `src/containers/firestream/supabase-realtime/{options.nix,module.nix}`,
  the flake-module, the `flake.nix` import and the CI profile entry.
- Type the env: `DB_HOST`, `DB_PORT`, `DB_USER`, `DB_PASSWORD`, `DB_NAME`,
  `DB_AFTER_CONNECT_QUERY`, `DB_ENC_KEY`, `API_JWT_SECRET`,
  `SECRET_KEY_BASE`, `SEED_SELF_HOST`, `RLIMIT_NOFILE`.
- Seed the self-host tenant on first start. Upstream uses `SEED_SELF_HOST`
  and the `realtime-dev` tenant name. The Envoy routes must match that name.
- Take secrets from [[FIRE-018]].

## Acceptance

- The image builds and the health endpoint answers.
- A websocket client gets a change event after an insert into a table with
  replication on.

## Spike findings

From the spike [[FIRE-010]]: run the release migrate step and the self-host seed before the server starts. Use `erlang_28` with `elixir_1_19`. The `pgdelta` tool (a Bun binary) for one dashboard page is not built; leave it out and record the gap. The working expression is `/tmp/firestream-supabase-spikes/FIRE-010/realtime.nix`.

## Work log

### 2026-10-22

Done. Realtime v2.143.3 builds from source and delivers change events.

Files:
- `src/containers/firestream/supabase-realtime/{options.nix,module.nix}`: container `realtime`, image `firestream-supabase-realtime:2.143.3` (372 MiB uncompressed). `module.nix` calls `firestream.mkMixRelease` with the [[FIRE-010]] pins (erlang_28, elixir_1_19, the mix deps hash and the assets npm hash) and wraps it with `mkBeamContainerModule`.
- `nix/flake-modules/containers/supabase-realtime.nix`: `packages.supabase-realtime`, `firestreamContainers.supabase-realtime`, `firestreamImages.supabase-realtime`. Also the `flake.nix` import and the `containerPackages` entry.

Boot: `preStart` does the upstream `run.sh` steps on every boot: `ulimit -Sn $RLIMIT_NOFILE`, `Realtime.Release.migrate()`, and with `SEED_SELF_HOST=true` `Realtime.Release.seeds(Realtime.Repo)`, which creates the `realtime-dev` tenant. Then `bin/realtime start` with `PHX_SERVER=true` (the upstream `bin/server`).

Env: the upstream compose values for `PORT`, `DB_HOST`, `DB_PORT`, `DB_USER=supabase_admin`, `DB_PASSWORD`, `DB_NAME`, `DB_AFTER_CONNECT_QUERY`, `DB_ENC_KEY`, `API_JWT_SECRET`, `METRICS_JWT_SECRET`, `SECRET_KEY_BASE`, `ERL_AFLAGS`, `DNS_NODES`, `RLIMIT_NOFILE`, `APP_NAME`, `SEED_SELF_HOST`, `RUN_JANITOR`, `DISABLE_HEALTHCHECK_LOGGING`, plus `DB_IP_VERSION=ipv4` (spike) and `PHX_SERVER=true`. The secrets have empty defaults; the entrypoint refuses to start without `DB_PASSWORD`, `DB_ENC_KEY`, `API_JWT_SECRET` and `SECRET_KEY_BASE`.

Verification (docker, network alias `realtime-dev.supabase-realtime`):
- `GET /healthcheck`: 200. healthd `/readyz`: 200. `GET /api/tenants/realtime-dev/health` with the anon JWT: `"healthy":true`.
- A websocket client (Python `websockets`, Host `realtime-dev.supabase-realtime`) joins `realtime:p2` with `postgres_changes` on `public.todos` and receives `system: Subscribed to PostgreSQL`. After an `insert` it receives `postgres_changes` with `INSERT todos {'id': 3, 'task': 'from-realtime-test'}`. The table was in publication `supabase_realtime`.

Deviations and gaps:
- The readiness probe is `GET /healthcheck`, not the upstream tenant health route, because that route needs the anon JWT.
- `pgdelta` (the Bun binary for the tenant-migration dashboard page) is not in the image. The realtime API does not use it.
- At boot, PromEx logs 4 metric-collection warnings (`ArgumentError`). No `[error]` lines.
- `API_JWT_JWKS` is not a default key. A consumer adds it for asymmetric keys.

For Phase 3: port 4000. The tenant id comes from the first label of the Host header, so the gateway Host rewrite must start with `realtime-dev.` (the Envoy image sets `ENVOY_REALTIME_TENANT_HOST`). Start after the database.
