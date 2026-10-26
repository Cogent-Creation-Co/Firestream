---
id: FIRE-058
title: "supabase-postgres reports ready only after the migrations finish"
type: feature
status: done
priority: high
tags: [supabase, postgresql]
affects: []
depends-on: []
---

# supabase-postgres reports ready only after the migrations finish

Epic: [[FIRE-009]]. Found in [[FIRE-037]].

## Problem

The `supabase-postgres` readiness check is `pg_isready -h 127.0.0.1`. On first boot, the init phase starts a temporary server on 127.0.0.1 (`postgresql_start_bg`) and runs the 64 Supabase migrations against it ([[FIRE-016]]). `pg_isready` passes during that time. So compose and Kubernetes start Auth and Realtime before the `_supabase` database and their schemas exist, and those services exit. Two of five FIRE-037 e2e runs failed this way.

## Scope

- Make the readiness check of the Supabase variant pass only after first-boot init is complete. Options:
  - a marker that init writes after the migrations, which the check tests;
  - a query for an object that the last migration creates;
  - a temporary init server that does not listen on TCP (socket only).

  Choose the one that works the same in compose (healthd `/readyz`) and in the chart's readiness probe.
- When the Supabase variant's options are not set, the stock postgresql image must stay unchanged. Prove this with a same-tree comparison.
- Check the dependent services in compose (`dependsOn` on db healthy) and in the chart (init containers or probes). Auth and Realtime must not start early.

## Acceptance

- A first boot with a fresh volume reports not-ready until every migration file has run, then ready.
- `make test-e2e-supabase` passes 5 times in a row on fresh volumes, with no service exit during `supabase-up`.
- `make test-e2e-k8s-supabase` passes.
- `checks.firestream-postgresql-extensions-seam` and `firestream-supabase-postgres-migrations` pass.

## Work log

### 2026-10-23

Work starts. Scope: make the Supabase db readiness pass only after first-boot init ends, in compose and in the chart.

### 2026-10-23

Design: a serving marker. The new option `postgresql.readyAfterInit` (default false) passes `readyMarker` to module.nix. Activation removes the marker on every boot. `runCmd` writes it just before `exec postgres`. The readiness command then requires the marker. Why: the marker exists only while the long-running server runs. A marker from init would also mark the temporary server ready during its shutdown. A socket-only init server needs changes to the shared helpers and to user init scripts. The chart probe and compose both use healthd `/readyz`, so one command covers both. The Supabase variant sets `readyAfterInit = true`.

Same-tree proof: the stock 16 and 17 images have equal drvPaths for HEAD, index and new module files. The Supabase image differs.

### 2026-10-23

Checks pass: `firestream-postgresql-extensions-seam` (7 assertions, two new) and `firestream-supabase-postgres-migrations` (64 files). The seam check now also sets `readyAfterInit = false` in the inert probe and asserts the Supabase readiness command needs the marker.

Direct first boot on a fresh volume: plain `pg_isready` passes at 1.7 s, during init. `/readyz` first returns 200 at 5.3 s. At that time all 64 migration files ran and init is complete. A restart is ready again in about 1 s and runs no migration.

### 2026-10-23

`nix flake check --no-build --all-systems` passes.

`make test-e2e-supabase` passes 5 times in a row on fresh volumes: `q0ao8j`, `yql0tn`, `1cgc1f`, `fit0hs`, `jkmowm`. Each run passes 9 tests. No service exits during `supabase-up`; only the one-shot `seaweedfs-buckets` job exits. The host has 0 containers and 14 volumes before and after.

### 2026-10-23

`make test-e2e-k8s-supabase` passes (cluster `dexdyl`). All pods become Ready, and all six Supabase probes pass. No k3d cluster is left.

FIRE-058 is complete.

**Files.**
- `src/containers/firestream/postgresql/options.nix`: a new option, `readyAfterInit` (default false). When it is true, the readiness command requires the serving marker.
- `src/containers/firestream/postgresql/module.nix`: a new argument, `readyMarker`. Activation removes the marker. `runCmd` writes it before `exec postgres`.
- `src/containers/firestream/postgresql/supabase.nix`: `readyAfterInit = true`.
- `nix/flake-modules/checks.nix`: the seam check asserts the inert default and the Supabase readiness command.

**Verification.**
- The stock 16 and 17 images have equal drvPaths for HEAD, index and new module files, in the same tree.
- Both checks pass. `nix flake check --no-build --all-systems` passes.
- A fresh first boot is ready only after all 64 migration files and init end.
- The Docker e2e passes 5 times in a row. The k8s e2e passes.

**Deviations.**
- The compose file and the chart do not change. Both use healthd `/readyz`. The chart init containers reach the db through a ClusterIP Service, which has no endpoint until the pod is Ready.
- The marker lives in `POSTGRESQL_TMP_DIR`. healthd does not see the env defaults, so the command uses the default path as a fallback.
