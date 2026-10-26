---
id: FIRE-032
title: "Remove import-from-derivation from the Supabase builds"
type: feature
status: done
priority: high
tags: [nix, phase-2, supabase]
affects: []
depends-on: [FIRE-023, FIRE-024, FIRE-025, FIRE-027]
---

# Remove import-from-derivation from the Supabase builds

Epic: [[FIRE-009]]

## Goal

Make every Supabase output evaluate without import-from-derivation (IFD), so
that `nix flake check --no-build --all-systems` passes again. Before Phase 2
it passed.

## Problem

`nix flake check --no-build --all-systems` fails with
`path '…-pnpm2nix-source.drv' is not valid`. Four IFD points exist:

1. `bin/nix/firestream/node/pnpm2nix.nix` imports `derivation.nix` from an
   `applyPatches` derivation.
2. pnpm2nix `lockfile.nix` parses the lockfile through
   `runCommand "toJSON" … yaml2json` and `fromJSON (readFile …)`. This
   affects postgres-meta, Storage and Studio. Studio also reads its lockfile
   from the fetched upstream source.
3. `bin/nix/firestream/beam/mkMixRelease.nix` reads the NIF `Cargo.lock` from
   the fetched source (`importCargoLock { lockFile = "${src}/…"; }`). This
   affects Supavisor.
4. `src/containers/firestream/postgres-meta/package.nix` reads
   `cargoLock.lockFile = oxc + "/Cargo.lock"` from a fetched source.

## Scope

- Vendor the patched pnpm2nix (ISC license; keep the license and the pinned
  upstream commit) into the repo. Remove the `pnpm2nix` flake input if
  nothing else needs it.
- Make the vendored code read a committed JSON lockfile. Commit
  `pnpm-lock.json` next to each `pnpm-lock.yaml` for postgres-meta, Storage
  and Studio. For Studio, commit the upstream lockfile pair at the pinned rev.
- Add a check that converts each YAML lockfile to JSON and compares it with
  the committed JSON. Add a script that regenerates the JSON files.
- Replace each `importCargoLock` of a fetched lockfile with
  `rustPlatform.fetchCargoVendor` and a hash. In `mkMixRelease`, the `nif`
  argument takes a `cargoHash`.
- Add `supabase-postgres` to `containerPackages` in
  `bin/nix/firestream/ci/profile.nix`. Phase 1 missed it.

## Acceptance

- `nix flake check --no-build --all-systems` passes.
- All nine Phase 2 images and `supabase-postgres` build. Each image's main
  acceptance test passes again in Docker.
- The new lockfile check passes, and it fails when a JSON lockfile is stale.
- The stock images are unchanged (same-tree comparison) where shared code
  changes.

## Work log

### 2026-10-22

IFD 1 and 2 fixed. The patched pnpm2nix is now in bin/nix/firestream/node/pnpm2nix/ (ISC license, upstream commit 9b15300b). It reads a committed pnpm-lock.json. The flake input pnpm2nix and the patch file are removed. IFD 3 and 4 fixed: mkMixRelease nif takes cargoHash (fetchCargoVendor), and the oxfmt binding uses cargoHash. A fifth IFD point was found: fenix.toolchainOf with sha256 in postgres-meta. It now uses fenix.fromManifestFile with builtins.fetchurl. nix flake check --no-build --all-systems passes.

### 2026-10-22

Added the check firestream-pnpm-lock-json and the script bin/nix/firestream/node/pnpm-lock-json.sh. The check passes. A temporary edit of the Storage JSON made it fail; the edit is reverted. The script is idempotent. Same-tree proof: the old and the vendored pnpm2nix give the same patched lockfile and the same dependency list for all three apps. mkMixRelease without nif gives the same drvPath for Realtime and beam-hello. The CI manifest changes only by supabase-postgres.

### 2026-10-22

Ticket done.

Files added:
- bin/nix/firestream/node/pnpm2nix/{derivation.nix,lockfile.nix,LICENSE}: the upstream files at 9b15300b with the platform-filter patch applied. lockfile.nix reads JSON. derivation.nix takes `pnpmLockJson`. pname and pnpmLockJson have no default from `src`.
- bin/nix/firestream/node/pnpm-lock-json.sh: writes pnpm-lock.json next to each pnpm-lock.yaml with the pinned remarshal.
- src/containers/firestream/{postgres-meta,supabase-storage,supabase-studio}/pnpm-lock.json, and the upstream Studio pnpm-lock.yaml at rev 36992789. Size: 3.70 MB.
- Check `firestream-pnpm-lock-json` in nix/flake-modules/checks.nix.

Files removed: bin/nix/firestream/node/pnpm2nix-platform-filter.patch and the flake input `pnpm2nix` (flake.lock is equal to HEAD again).

Files changed: pnpm2nix.nix (no `src` argument), three flake-modules, three package.nix files, mkMixRelease.nix (`nif.cargoHash`, fetchCargoVendor), supavisor/module.nix, ci/profile.nix (`supabase-postgres`).

Verification:
- `nix flake check --no-build --all-systems` passes.
- All ten images build. The Envoy binary did not rebuild.
- The lock check passes. It fails on a stale Storage JSON. The edit is reverted.
- Docker against supabase-postgres: postgres-meta `/tables` lists auth.users and the probe table, and typegen returns 200. Storage: bucket 200, upload 200, download 200, sha256 equal, the object is in SeaweedFS. Studio `/api/platform/profile` 200 and pg-meta tables through Studio. Supavisor psql through 5432 and 6543 returns the probe count. No error lines.
- Same-tree proof: the old and vendored pnpm2nix give the same patched lockfile and dependency list for all three apps. mkMixRelease without `nif` gives the same drvPath for Realtime and beam-hello. The CI manifest changes only by the supabase-postgres entry.

Deviations:
- A fifth IFD point existed: `fenix.toolchainOf` with sha256 in postgres-meta. It now uses `fenix.fromManifestFile` with `builtins.fetchurl` and the same hash.
- The oxfmt binding uses `cargoHash`, which is fetchCargoVendor in buildRustPackage.
- Studio fails its build when the committed pnpm-lock.yaml differs from the lockfile in `src`.

Found, not fixed: the stock seaweedfs image fails under `docker run` with no command: the entrypoint runs `exec exec weed server ...` and exits 127. The test gave the command explicitly. The chart path is not affected.
