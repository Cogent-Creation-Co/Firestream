---
id: FIRE-014
title: "Package the supautils, supabase_vault and plan_filter Postgres extensions"
type: feature
status: done
priority: high
tags: [phase-1, postgresql, supabase]
affects: []
depends-on: [FIRE-012]
---

# Package the supabase_vault Postgres extension

Epic: [[FIRE-009]]

## Goal

Build `supabase/vault` as a Postgres 17 extension with Nix. The Supabase
migrations need it, and nixpkgs does not have it.

## Scope

- Add a package under `bin/nix/firestream/packages/` and list it in the
  package index.
- Build it against `postgresql_17` from our nixpkgs pin. Use the
  `postgresql.pkgs` extension builder pattern from nixpkgs.
- Add any other missing extension that the spike [[FIRE-012]] marks as
  required. Leave optional extensions (`pg_jsonschema`, `index_advisor`,
  `pg_hashids`) out.

## Acceptance

- The package builds on x86_64-linux and aarch64-linux.
- `CREATE EXTENSION supabase_vault` works in a test database.
- A Nix check covers the build.

## Spike findings

The spike [[FIRE-012]] changes the scope. Package three extensions, not one:

- `supautils` 3.2.2 (`supabase/supautils` tag `v3.2.2`). This is mandatory. Without it, every `authenticator` login fails, so PostgREST cannot connect.
- `supabase_vault` 0.3.1 (`supabase/vault` tag `v0.3.1`, needs `libsodium`).
- `plan_filter` (`pgexperts/pg_plan_filter`). The old repo name `plan_filter` returns 404.

Build with nixpkgs `postgresqlBuildExtension.nix` and `{ postgresql = pg; stdenv = pg.stdenv; }`. The pg17 stdenv is clang, so a gcc stdenv fails. The hashes and a working expression are in the FIRE-012 Result section and in `/tmp/firestream-supabase-spikes/FIRE-012/postgres.nix`.

## Work log

### 2026-10-22

Done. The three extensions build against `postgresql_17` 17.7 on x86_64-linux.

Files:
- `bin/nix/firestream/packages/postgresql-extensions/{default,supautils,supabase-vault,plan-filter}.nix`.
- `bin/nix/firestream/packages/default.nix` exposes `postgresqlExtensionsFor`. It is a function from a `postgresql_<N>` package to the set `{ supautils; supabase_vault; plan_filter; }`.
- `nix/flake-modules/checks.nix` adds `checks.<sys>.firestream-postgresql-supabase-extensions`.

Verification:
- `nix build .#checks.x86_64-linux.firestream-postgresql-supabase-extensions` passes. The check creates `supabase_vault` and reads back a secret through `vault.decrypted_secrets`. It reads the `plan_filter` setting. It counts the `supautils.*` settings that the session preload creates.
- The same test also passed in a manual initdb outside the sandbox.
- aarch64-linux: the three derivations evaluate. This host has no aarch64 builder, so the aarch64 build is not proven.

supautils and plan_filter have no control file. They load through `session_preload_libraries` and `shared_preload_libraries`, not through `CREATE EXTENSION`.
