---
id: FIRE-024
title: "postgres-meta image"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: []
---

# postgres-meta image

Epic: [[FIRE-009]]

## Goal

Build `supabase/postgres-meta` v0.100.0 with Nix and package it as a
firestream image. Studio uses it to manage the database.

## Scope

- Build with `firestream.mkNodePackage` and wrap it with
  `mkNodeContainerModule`.
- Add `src/containers/firestream/postgres-meta/{options.nix,module.nix}`,
  the flake-module, the `flake.nix` import and the CI profile entry.
- Type the env: `PG_META_PORT`, `PG_META_DB_HOST`, `PG_META_DB_PORT`,
  `PG_META_DB_NAME`, `PG_META_DB_USER`, `PG_META_DB_PASSWORD`,
  `CRYPTO_KEY`.

## Acceptance

- The image builds and `GET /health` answers.
- `GET /tables` lists the tables of the Supabase Postgres image.

## Node build decision

Use pnpm2nix (see the decisions in [[FIRE-009]]). Generate a `pnpm-lock.yaml` from the upstream `package-lock.json` with `pnpm import`. Build native modules from source.

## Work log

### 2026-10-22

Image done: `packages.postgres-meta` builds `firestream-supabase-postgres-meta:0.100.0` (container name `pgmeta`).

Build route: pnpm2nix (input `pnpm2nix`, the FliegendeWurst fork of nzbr/pnpm2nix-nzbr at 9b15300b, with `bin/nix/firestream/node/pnpm2nix-platform-filter.patch`). Each locked package is one fixed-output fetch. The lockfile `pnpm-lock.yaml` is `pnpm import` (pnpm 10.26.1) of the upstream package-lock.json. Node 22. Install scripts do not run.

Native modules, built from source:
- `@sentry-internal/node-cpu-profiler` 2.1.0: node-gyp from the C++ source in the npm tarball.
- `@oxfmt/binding-linux-x64-gnu` 0.66.0: cargo build of oxc tag `oxfmt_v0.66.0`, with a pinned fenix toolchain 1.98.0 (oxc needs Rust 1.96; our pins have 1.91 and 1.92).
- `@napi-rs/nice` is removed. piscina ignores a load failure.
- A build check fails on any other `.node` file.

Files:
- src/containers/firestream/postgres-meta/{options.nix,module.nix,package.nix,pnpm-lock.yaml}
- nix/flake-modules/containers/postgres-meta.nix, its flake.nix import, the `pnpm2nix` flake input (flake.lock changed)
- bin/nix/firestream/node/{pnpm2nix.nix,pnpm2nix-platform-filter.patch}
- bin/nix/firestream/containers/node.nix: pass-through of imageName, imageTag, health
- `postgres-meta` in containerPackages (bin/nix/firestream/ci/profile.nix)

Verification (docker network with `.#supabase-postgres`, `--read-only --tmpfs /tmp`):
- `GET /health` 200. healthd `/readyz` 200.
- `GET /tables` lists `auth.users` and a test table `public.fire_node_probe`.
- `GET /generators/typescript?included_schemas=public` 200; it loads the oxfmt binding.
- The node.nix change: the nextjs image drvPath is the same with the old and the new node.nix in one tree (/tmp/fire-node-compare.nix).

Closure: 350 MiB (app 140 MiB, Node.js 210 MiB).

Open items:
- `libpg-query` (via pgsql-parser) ships a prebuilt WASM file from npm. It is not a `.node` file. Building it needs emscripten.
- Port 8080. Env: PG_META_DB_HOST/PORT/NAME/USER/PASSWORD, CRYPTO_KEY (= PG_META_CRYPTO_KEY). Readiness: GET /health on PG_META_PORT.
