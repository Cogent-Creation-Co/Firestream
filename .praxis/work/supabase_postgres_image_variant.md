---
id: FIRE-015
title: "Postgres extensions option and Supabase image variant"
type: feature
status: done
priority: high
tags: [phase-1, postgresql, supabase]
affects: []
depends-on: [FIRE-012, FIRE-014]
---

# Postgres extensions option and Supabase image variant

Epic: [[FIRE-009]]

## Goal

Let the firestream `postgresql` image ship Postgres extensions. Then build a
Supabase variant of the image with the extension set from [[FIRE-012]].

## Scope

- Add a typed `extensions` option to
  `src/containers/firestream/postgresql/options.nix`.
- In `module.nix`, use `postgresql.withPackages` when the option is set.
  Today the module uses the plain `pkgs."postgresql_${version}"`.
- Set `POSTGRESQL_SHARED_PRELOAD_LIBRARIES` for the variant from the spike
  result.
- Register the variant as its own image, `firestreamImages.supabase-postgres`.
- Include `supabase_vault` from [[FIRE-014]].

## Acceptance

- When the option is not set, the stock postgresql image is byte-identical.
  A check asserts equal drvPaths, like
  `checks.firestream-python-workspace-seam`.
- The variant image starts, and each extension in the set passes
  `CREATE EXTENSION`.
- `checks.postgresql-render-fidelity` stays green.

## Spike findings

From the spike [[FIRE-012]] (see its Result section for the full lists):

- The nixpkgs extension set: pgsodium, pg_graphql, pg_net, pgjwt, pg_cron, pgvector, wal2json, plpgsql_check, pgaudit, postgis, pg_repack, pg_safeupdate, pg_tle, pgmq, pgsql-http, hypopg, pgtap. Add the three from [[FIRE-014]].
- `shared_preload_libraries` is the upstream list without `timescaledb`.
- `session_preload_libraries = 'supautils'`, with the upstream `supautils.conf` included. Remove `timescaledb` and `plv8` from it.
- `pgsodium.getkey_script` and `vault.getkey_script` print a persisted 32-byte hex key. The key must be on the persistent volume. A lost key makes vault secrets unreadable.
- `wal_level = logical` (Realtime needs it) and `cron.database_name = 'postgres'`.
- Use `initdb -U supabase_admin`. The migration script creates `postgres`.

## Work log

### 2026-10-22

Done. The postgresql container has two new options. A new image, `firestreamImages.supabase-postgres`, uses them.

Files:
- `src/containers/firestream/postgresql/options.nix`: `extensions` (null or `ps: [ ... ]`) and `extraConf` (lines). Both go to `module.nix` through `extraModuleArgs`.
- `src/containers/firestream/postgresql/module.nix`: uses `withPackages` only when `extensions` is set. The extension set is `postgresql.pkgs` plus `postgresqlExtensionsFor`. The activate phase writes `extraConf` to `conf.d/firestream.conf` only when it is not empty.
- `src/containers/firestream/postgresql/supabase.nix`: the Supabase options layer. It sets 20 extensions, the preload list, `wal_level = logical`, supautils, the pgsodium/vault getkey script, `cron.database_name` and the image name.
- `nix/flake-modules/containers/supabase-postgres.nix`, and its import in `flake.nix`.
- `bin/nix/firestream/supabase/default.nix`: upstream pins (`supabase/postgres@17.6.1.136`, sparse `supabase/supabase@self-hosted/v0.8.3` `docker/volumes`), exposed as `firestream.supabase.sources`.
- `nix/flake-modules/checks.nix`: `firestream-postgresql-extensions-seam`.

Deviation: the upstream files are fetched by hash, not copied into the repo. An upstream sync changes one rev and one hash. The supautils config is the upstream file unchanged except for the custom-scripts path. The image works with it.

The pgsodium root key: the getkey script prints `POSTGRESQL_PGSODIUM_ROOT_KEY` when it is set (`_FILE` works too). Otherwise it reads or creates `$POSTGRESQL_VOLUME_DIR/pgsodium_root.key`. The chart must keep that path on the persistent volume.

Verification:
- Stock images are unchanged: the HEAD `module.nix` + `options.nix` and the new ones give equal drvPaths for versions 16 and 17 in the same tree. A drvPath comparison across two tree states is not valid, because the Rust tools in every image use the whole repo as source.
- `checks.x86_64-linux.firestream-postgresql-extensions-seam`: PASS (6 assertions).
- `checks.x86_64-linux.postgresql-render-fidelity`: PASS.
- `nix build .#supabase-postgres`, then `docker run` with a volume: the server starts, pgaudit, pgsodium, vault and pg_cron load, and the key file is created with mode 0600.
- All 17 extensions pass `CREATE EXTENSION`: pgsodium, pg_graphql, pg_net, pgjwt, pg_cron, vector, plpgsql_check, pgaudit, postgis, pg_repack, pg_tle, pgmq, http, hypopg, pgtap, supabase_vault, pg_stat_statements. wal2json creates a logical slot. safeupdate loads. A vault secret round-trips.
- Recreate on the same volume: the key is kept, `wal_level` is `logical`, and the vault secret still decrypts.

For [[FIRE-016]]: supautils runs the upstream before-create and after-create scripts. They need the `supabase_admin` and `service_role` roles. Before the migrations create them, `CREATE EXTENSION` fails. In this test I created both roles by hand.

### 2026-10-22

Follow-up from [[FIRE-016]]:
- The activate-phase additions now come from one `variantActivation` binding, so the default script stays byte-identical. A `WARNING` comment records this.
- `supautilsConf` moved to `firestream.supabase.postgres`.
- The stock comparison for versions 16 and 17 passed again after these changes.
