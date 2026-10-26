---
id: FIRE-016
title: "Vendor the Supabase init SQL and migrations, and run them in initFn"
type: feature
status: done
priority: high
tags: [phase-1, postgresql, supabase]
affects: []
depends-on: [FIRE-015]
---

# Vendor the Supabase init SQL and migrations, and run them in initFn

Epic: [[FIRE-009]]

## Goal

Make the Supabase Postgres image ([[FIRE-015]]) create the Supabase roles,
schemas and grants on first start.

## Scope

- Vendor the migrations from `supabase/postgres` (`migrations/db/`) at a
  pinned commit. Vendor the init SQL from `supabase/supabase`
  (`docker/volumes/db/`: `roles.sql`, `jwt.sql`, `realtime.sql`,
  `webhooks.sql`, `_supabase.sql`, `pooler.sql`). Leave `logs.sql` out,
  because Logflare is out of scope.
- Run them in order from the image's `initFn`, once, on an empty data
  directory.
- Read the role passwords and the JWT secret from env and `_FILE` secrets,
  through the existing `mkAppModule` secret loaders.
- Record the pinned commits so that an upstream sync is a clear task.

## Acceptance

- A fresh container ends with the schemas `auth`, `storage`, `realtime`,
  `extensions`, `graphql_public` and `_realtime`, and the roles `anon`,
  `authenticated`, `service_role`, `authenticator` and `supabase_admin`.
- A restart does not run the migrations again.
- A Nix or e2e test asserts the schemas and roles.

## Spike findings

From the spike [[FIRE-012]]: run the upstream `migrations/db/migrate.sh` unchanged against the vendored tree. Run it only on first init. The Result section of FIRE-012 lists the files to vendor and their pinned refs (`supabase/postgres@17.6.1.136`, `supabase/supabase@self-hosted/v0.8.3`). `99-jwt.sql` needs `JWT_EXP`. `/tmp/firestream-supabase-spikes/FIRE-012/run-migrations.sh` is a working reference. Leave `99-logs.sql` out, because Logflare is out of scope; check first that no later file needs it.

## Work log

### 2026-10-22

Done. The Supabase Postgres image applies the upstream migrations on first boot.

Files:
- `bin/nix/firestream/supabase/default.nix`: `postgres.migrations` builds the upstream `/docker-entrypoint-initdb.d` tree from `supabase/postgres@17.6.1.136`. It adds the compose mounts of `self-hosted/v0.8.3`: webhooks, roles, jwt, `_supabase`, realtime and pooler. `logs.sql` stays out. `postgres.supautilsConf` moved here too.
- `src/containers/firestream/postgresql/options.nix` and `module.nix`: a new option, `initdbSuperuser` (default `postgres`). Any other value replaces `postgresql_master_init_db` in the activate phase. The new helper runs `initdb -U <user>` and then creates a `postgres` superuser in single-user mode.
- `src/containers/firestream/postgresql/supabase.nix`: `initdbSuperuser = "supabase_admin"`. `POSTGRESQL_INITSCRIPTS_DIR` points at a baked directory with `00-supabase-migrate.sh` and `99-user-init-scripts.sh`. The second file still runs a `/docker-entrypoint-initdb.d` mount, as `postgres`. Other settings: `ALLOW_EMPTY_PASSWORD = no` and `JWT_EXP = 3600`.
- `nix/flake-modules/checks.nix`: `firestream-supabase-postgres-migrations`.

Why the bootstrap user changed: migration `10000000000000_demote-postgres.sql` fails with "The bootstrap superuser must have the SUPERUSER attribute" when `postgres` is the initdb user. Upstream uses `supabase_admin` as the initdb user. `migrate.sh` itself runs unchanged.

Verification:
- `checks.x86_64-linux.firestream-supabase-postgres-migrations`: PASS. 64 files apply on PostgreSQL 17.7, with the image's server, extensions and settings. The check asserts 7 schemas, 7 roles, the `_supabase` database, `postgres` not superuser, and `authenticator` to `anon`.
- `docker run` of the final image on a fresh volume: 64 files run with 0 errors. These exist: the schemas `_realtime auth extensions graphql graphql_public pgbouncer realtime storage vault`, and the roles `anon authenticated authenticator dashboard_user pgbouncer service_role supabase_admin supabase_auth_admin supabase_privileged_role supabase_storage_admin`. `app.settings.jwt_exp` is 3600. `authenticator` logs in and `SET ROLE anon` works. Vault works as `postgres`.
- Recreate on the same volume: 0 migration files run, `wal_level` is `logical`, and a Vault secret still decrypts.
- The stock images are unchanged: the HEAD and new module give equal drvPaths for versions 16 and 17.

Open items for later tickets:
- `postgres` is not a superuser after the migrations. Any chart job that expects a superuser (for example `pg_dumpall`) must use `supabase_admin`. See [[FIRE-029]].
- Every Supabase service role gets `POSTGRES_PASSWORD` (upstream `roles.sql`). Separate passwords for each service are a production follow-on.
