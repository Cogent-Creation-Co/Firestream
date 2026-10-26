---
id: FIRE-012
title: "Spike: Supabase migrations on the firestream Postgres image"
type: research
status: done
priority: high
tags: [phase-0, spike, supabase]
affects: []
depends-on: []
---

# Spike: Supabase migrations on the firestream Postgres image

Epic: [[FIRE-009]]

## Goal

Find out if the Supabase init SQL and migrations apply cleanly on
`pkgs.postgresql_17` with extensions from our nixpkgs pin (`release-25.11`).

## Questions

- Do the migrations in `supabase/postgres` (`migrations/db/`) and the init
  SQL in `supabase/supabase` (`docker/volumes/db/`) apply in order without
  errors?
- nixpkgs has `pgsodium`, `pg_graphql`, `pg_net`, `pgjwt`, `pg_cron`,
  `pgvector`, `wal2json`, `plpgsql_check`, `pgaudit`, `postgis` and
  `pg_repack`. nixpkgs does not have `supabase_vault`, `pg_jsonschema`,
  `index_advisor` or `pg_hashids`. Is `supabase_vault` the only missing
  extension that the migrations need?
- Which `shared_preload_libraries` are necessary?
- Which roles and passwords must exist before the migrations run
  (`supabase_admin`, `authenticator`, `supabase_auth_admin` and others)?
- Do the extension versions in nixpkgs differ from
  `nix/ext/versions.json` in `supabase/postgres` in a way that matters?

## Scope

- A local test with `postgresql_17.withPackages`. No container change.

## Exit

Write the result in the ticket log: "go", "go with changes" or "no-go and
why". List the extensions, the preload libraries and the migration files
that we must vendor. FIRE-014, FIRE-015 and FIRE-016 use this result.

## Result

**Verdict: go with changes.** All 52 migrations and the self-host init SQL apply without errors on `postgresql_17` 17.7 from `release-25.11`. But **supautils is mandatory**, not only `supabase_vault`. We must package three extensions that nixpkgs does not have: `supautils`, `supabase_vault` and `plan_filter`. Each is a small C/PGXS build.

Scratch: `/tmp/firestream-supabase-spikes/FIRE-012/postgres.nix` (attributes `postgres` and `postgresNixOnly`) and `run-migrations.sh <full|nixonly>`.

### Test method

`run-migrations.sh` copies the upstream image layout. It runs `initdb -U supabase_admin`, writes the config, and runs the upstream `migrations/db/migrate.sh` unchanged. The `initdb.d` tree holds `supabase/postgres@17.6.1.136` `migrations/db/`, `pgbouncer_auth_schema.sql` as `init-scripts/00-schema.sql`, `stat_extension.sql` as `migrations/00-extension.sql`, and the compose mounts from `supabase/supabase@self-hosted/v0.8.3` `docker/volumes/db/` (98-webhooks, 99-roles, 99-jwt, 97-_supabase, 99-realtime, 99-logs, 99-pooler).

### Results

- Full mode: `migrate.sh` exit 0. 65 files ran. No ERROR. The `_supabase` database, the `vault` schema and 15 roles exist. `vault.create_secret` and `vault.decrypted_secrets` work. `authenticator` logs in with `session_preload_libraries = supautils, safeupdate`. The FIRE-010 Realtime and Supavisor releases migrate and run against this database.
- nixpkgs-only mode: `migrate.sh` also exits 0. Then every `authenticator` login fails with `FATAL: could not access file "supautils"`. Migration `20260413000000_fix-authenticator-session-preload-libraries.sql` sets this without a check. So PostgREST cannot connect without supautils.
- Without vault, migration `20221207154255` falls back to `pgsodium` 3.1.9 and creates no `vault` schema. Studio's Vault page and `vault.*` calls then fail.
- With vault 0.3.1, the migration creates `supabase_vault` and does not create `pgsodium`.

### Extensions to package (outside nixpkgs)

Build with nixpkgs' `pkgs/servers/sql/postgresql/postgresqlBuildExtension.nix`. Call it with `{ postgresql = pg; stdenv = pg.stdenv; }`. The pg17 stdenv is clang, and PGXS fails with plain gcc stdenv (`clang: command not found`). The builder is not exported from `postgresql_17.pkgs`.

| Extension | Version | Source | Hash |
|---|---|---|---|
| supautils | 3.2.2 | `supabase/supautils` tag `v3.2.2` | `sha256-Oi35fel2Yp58eHsWVOXtxAo/s0RAbUxJEBxPrYRK+cs=` |
| supabase_vault | 0.3.1 | `supabase/vault` tag `v0.3.1`, needs `libsodium` | `sha256-MC87bqgtynnDhmNZAu96jvfCpsGDCPB0g5TZfRQHd30=` |
| plan_filter | 0.1 | `pgexperts/pg_plan_filter` rev `5081a7b5…` | `sha256-YNeIfmccT/DtOrwDmpYFCuV2/P6k3Zj23VWBDkOh6sw=` |

The upstream recipe names the repo `pgexperts/plan_filter`. That URL now returns 404. Use `pg_plan_filter`.

Not needed by the migrations: `pg_jsonschema`, `index_advisor`, `pg_hashids`, `wrappers`, `pg_stat_monitor`, `orioledb`. Their migrations are conditional. Users that run `create extension` on them get "not available".

### nixpkgs extensions used

pgsodium 3.1.9, pg_graphql 1.5.12-unstable, pg_net 0.20.2, pgjwt 0-unstable-2023-03-02, pg_cron 1.6.7, pgvector 0.8.1, wal2json 2.6, plpgsql_check 2.8.5, pgaudit 17.1, postgis 3.6.1, pg_repack 1.5.3, pg_safeupdate 1.5, pg_tle 1.5.2, pgmq 1.7.0, pgsql-http 1.7.0, hypopg 1.4.2, pgtap 1.3.4.

### Version differences against `nix/ext/versions.json`

- postgis: nixpkgs 3.6.1, upstream 3.3.7. This matters only to restore a dump from Supabase Cloud with PostGIS objects. A fresh install is fine.
- pgmq 1.7.0 vs 1.5.1, pg_tle 1.5.2 vs 1.4.0, pg_cron 1.6.7 vs 1.6.4, pgsodium 3.1.9 vs 3.1.8, pg_net 0.20.2 vs 0.20.3, vector 0.8.1 vs 0.8.2, pg_graphql 1.5.12 vs 1.5.11. All are minor. The migrations accept pgsodium 3.1.9 explicitly.
- Postgres itself: nixpkgs 17.7, upstream 17.6. No effect.

### Configuration

- `shared_preload_libraries = 'pg_stat_statements, pgaudit, plpgsql, plpgsql_check, pg_cron, pg_net, pgsodium, auto_explain, pg_tle, plan_filter, supabase_vault'` (upstream list without `timescaledb`).
- `session_preload_libraries = 'supautils'` and `include` of the upstream `supautils.conf` (remove `timescaledb` and `plv8`, fix `extension_custom_scripts_path`). Ship `postgresql_extension_custom_scripts/`.
- `pgsodium.getkey_script` and `vault.getkey_script`: a script that prints a persisted 32-byte hex key. The key must live on the PVC. A lost key makes vault secrets unreadable.
- `cron.database_name = 'postgres'`, `wal_level = logical` (Realtime needs it).

### Roles and passwords

- Before the migrations: superuser `supabase_admin` (initdb user, `POSTGRES_USER`). `migrate.sh` creates superuser `postgres` with `POSTGRES_PASSWORD` and then sets the `supabase_admin` password.
- The init scripts create `anon`, `authenticated`, `service_role`, `authenticator`, `supabase_auth_admin`, `supabase_storage_admin`, `supabase_functions_admin`, `supabase_replication_admin`, `supabase_read_only_user`, `supabase_etl_admin`, `supabase_privileged_role`, `dashboard_user` and `pgbouncer`.
- `99-roles.sql` sets the password of `authenticator`, `pgbouncer`, `supabase_auth_admin`, `supabase_functions_admin` and `supabase_storage_admin` from `POSTGRES_PASSWORD`. `99-jwt.sql` needs `JWT_EXP`.

### Files to vendor

From `supabase/postgres@17.6.1.136`: `migrations/db/` (migrate.sh, init-scripts, migrations), `ansible/files/pgbouncer_config/pgbouncer_auth_schema.sql`, `ansible/files/stat_extension.sql`, `ansible/files/postgresql_config/supautils.conf.j2`, `ansible/files/postgresql_config/conf.d/*`, `ansible/files/postgresql_extension_custom_scripts/`. From `supabase/supabase@self-hosted/v0.8.3`: `docker/volumes/db/*.sql` and `docker/volumes/pooler/pooler.exs`.

### Impact on downstream tickets

- FIRE-014: add `supautils`, `supabase_vault` and `plan_filter` derivations. supautils is a hard requirement. No nixpkgs bump.
- FIRE-015: use the preload list, the supautils include and the getkey script above. Persist the key file.
- FIRE-016: run the upstream `migrate.sh` unchanged with the vendored tree. It is idempotent only on first init (initdb.d semantics).

## Work log

### 2026-10-22

Spike done. Verdict: go with changes. Migrations apply cleanly. supautils, supabase_vault and plan_filter must be packaged; supautils is mandatory. See the Result section.
