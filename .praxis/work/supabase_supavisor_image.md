---
id: FIRE-027
title: "Supavisor image and pooler tenant seed"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: [FIRE-016, FIRE-017]
---

# Supavisor image and pooler tenant seed

Epic: [[FIRE-009]]

## Goal

Build `supabase/supavisor` 2.9.13 with Nix and package it with the BEAM
factory. Make the pooler tenant setup automatic.

## Scope

- Build the release with the method from the spike [[FIRE-010]] and wrap it
  with the BEAM factory from [[FIRE-017]].
- Add `src/containers/firestream/supavisor/{options.nix,module.nix}`, the
  flake-module, the `flake.nix` import and the CI profile entry.
- Type the env: `DATABASE_URL`, `SECRET_KEY_BASE`, `VAULT_ENC_KEY`,
  `API_JWT_SECRET`, `METRICS_JWT_SECRET`, `POOLER_TENANT_ID`,
  `POOLER_DEFAULT_POOL_SIZE`, `POOLER_MAX_CLIENT_CONN`, `POOLER_POOL_MODE`.
- Seed the pooler tenant on first start. Upstream does this with
  `docker/volumes/pooler/pooler.exs`; it uses the `_supabase` database from
  [[FIRE-016]].
- Take secrets from [[FIRE-018]].

## Acceptance

- The image builds and the health endpoint answers.
- `psql` connects through the pooler in transaction mode and in session
  mode.

## Spike findings

From the spike [[FIRE-010]]: run the release migrate step and the pooler tenant seed before the server starts. Use `erlang_27` with `elixir_1_18`. The rustler NIF (`pg_query`) builds offline from vendored Cargo deps. The working expression is `/tmp/firestream-supabase-spikes/FIRE-010/supavisor.nix`.

## Work log

### 2026-10-22

Done. Supavisor 2.9.13 builds from source (with the Rust NIF) and pools connections in both modes.

Files:
- `src/containers/firestream/supavisor/{options.nix,module.nix}`: container `supavisor`, image `firestream-supabase-supavisor:2.9.13`. `module.nix` calls `firestream.mkMixRelease` with the [[FIRE-010]] pins (erlang_27, elixir_1_18, the mix deps hash, `nif.cargoRoot = "native"`) and wraps it with `mkBeamContainerModule`.
- `nix/flake-modules/containers/supavisor.nix`: `packages.supavisor`, `firestreamContainers.supavisor`, `firestreamImages.supavisor`. Also the `flake.nix` import and the `containerPackages` entry.

Boot: `preStart` runs `Supavisor.Release.migrate()`, then `eval` of the upstream `docker/volumes/pooler/pooler.exs` (from `firestream.supabase.sources.dockerVolumes`). The seed creates the tenant only when it does not exist. Then `bin/supavisor start` with `PHX_SERVER=true`.

Env: the upstream compose values. `POOLER_TENANT_ID` defaults to `firestream` (upstream has a placeholder). An empty `DATABASE_URL` is built at boot as `ecto://supabase_admin:$POSTGRES_PASSWORD@$POSTGRES_HOST:$POSTGRES_PORT/_supabase`, like upstream. The secrets have empty defaults; the entrypoint refuses to start without `POSTGRES_PASSWORD`, `SECRET_KEY_BASE`, `VAULT_ENC_KEY`, `API_JWT_SECRET` and `METRICS_JWT_SECRET`.

Verification (docker, against supabase-postgres):
- `GET /api/health`: 204. healthd `/readyz`: 200.
- `psql -h supavisor -p 5432 -U postgres.firestream` (session mode) and `-p 6543` (transaction mode): both return `count(*) from todos`. `_supavisor.users` in `_supabase` holds tenant `firestream`, user `pgbouncer`, `mode_type = transaction`.
- A restart over the same database boots again (the seed skips the existing tenant) and both ports answer. No `[error]` lines.

Deviations:
- The image adds `pkgs.hostname`. The release `env.sh` names the node `supavisor@$NODE_IP`, else `supavisor@$(hostname -f)`, and fails with exit 127 without it.
- The readiness probe accepts 200 and 204, because `/api/health` answers 204.

For Phase 3: ports 4000 (API), 5432 (session), 6543 (transaction). Clients use the user `<role>.<POOLER_TENANT_ID>`. The upstream compose publishes 5432 and 6543 on the host. Start after the database.
