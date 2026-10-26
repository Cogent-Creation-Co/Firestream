---
id: FIRE-020
title: "PostgREST image"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: [FIRE-018]
---

# PostgREST image

Epic: [[FIRE-009]]

## Goal

Package PostgREST as a firestream image for the Supabase REST API.

## Scope

- Use `pkgs.postgrest` (14.12 in our pin). Upstream Supabase pins v14.18.
  Decide if 14.12 is acceptable. If not, override the version.
- Add `src/containers/firestream/postgrest/{options.nix,module.nix}` and
  `nix/flake-modules/containers/postgrest.nix`. Add the import to
  `flake.nix`.
- Type the main env: `PGRST_DB_URI`, `PGRST_DB_SCHEMAS`,
  `PGRST_DB_ANON_ROLE`, `PGRST_JWT_SECRET`, `PGRST_DB_USE_LEGACY_GUCS`.
  Take secrets from [[FIRE-018]].
- Add the image to `containerPackages` in the CI profile.

## Acceptance

- The image builds and serves `GET /` with the anon key against the
  Supabase Postgres image.
- The image runs with a read-only root filesystem.

## Work log

### 2026-10-22

Done. The PostgREST image builds and serves the REST API.

Files:
- `src/containers/firestream/postgrest/{options.nix,module.nix}`: container `postgrest`, image `firestream-supabase-postgrest:14.1`.
- `nix/flake-modules/containers/postgrest.nix`: `packages.postgrest`, `firestreamContainers.postgrest`, `firestreamImages.postgrest`.
- `flake.nix` (import) and `bin/nix/firestream/ci/profile.nix` (`containerPackages`).

Decision: use the pin's `pkgs.postgrest` 14.1. Upstream runs v14.18. Version 14.1 accepts all upstream `PGRST_*` keys. It ignores `PGRST_DB_USE_LEGACY_GUCS`, because it reads only the non-legacy GUCs, which is the upstream value.

Verification (docker, network `fire-p2-net`, `--read-only`):
- `GET /` with the anon key: 200.
- `GET /todos` (a table with `grant select to anon`): 200 with rows, for `ANON_KEY`, `ANON_KEY_ASYMMETRIC` and `SERVICE_ROLE_KEY` when `PGRST_JWT_SECRET` is `JWT_JWKS`. A bad JWT gives 401.
- healthd `GET :9180/readyz`: 200. No error lines in the log.

Deviations:
- The readiness probe is an HTTP request to the admin port 3001 `/ready`, not `postgrest --ready`. Version 14.1 refuses `--ready` unless `server-host` is `localhost`.
- The image sets `enableStateTracking = false`, because it keeps no state. This removes a read-only file system message at boot.
- Secrets have empty defaults. Compose and the chart must set `PGRST_DB_URI` (`postgres://authenticator:<POSTGRES_PASSWORD>@<db>:5432/postgres`) and `PGRST_JWT_SECRET` (`JWT_JWKS` or `JWT_SECRET`). Both accept `_FILE`.

Phase 1 defect found and fixed: database `postgres` was owned by `supabase_admin`, so the `postgres` role could not create tables in `public`. Upstream `migrate.sh` changes the owner only when it creates the role, and the Firestream image pre-creates it. `src/containers/firestream/postgresql/supabase.nix` now runs `alter database postgres owner to postgres` in its migrate wrapper. After the fix, `datdba` is `postgres` and `create table public.todos` as `postgres` works.

For Phase 3: port 3000 (API), 3001 (admin, localhost only), 9180 (healthd).
