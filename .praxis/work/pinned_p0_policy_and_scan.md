---
id: FIRE-042
title: "P0: pinning policy doc and the no-prebuilt-binaries check"
type: feature
status: done
priority: high
tags: [nix, supabase]
affects: []
depends-on: []
---

# P0: pinning policy doc and the no-prebuilt-binaries check

Epic: [[FIRE-041]]

## Scope

1. **Policy doc:** `docs/nix-pinning-policy.md`. It states:
   - what is allowed: source, a GitHub commit SHA plus a hash, registry source pinned by a lockfile hash, nixpkgs;
   - what is forbidden: prebuilt binaries outside nixpkgs, moving refs, and monolithic package-manager FODs where a per-package fetch exists;
   - the trusted base: nixpkgs (its seeds and cache.nixos.org) and fenix;
   - how to add an exception: a `TODO(<ticket>)` comment plus an allowlist entry.
2. **The check `checks.<sys>.supabase-no-prebuilt-binaries`:**
   - It scans the app derivation output of each Supabase image for ELF, Mach-O, PE and WASM magic bytes: the `node_modules` trees, the release `lib/` trees, the extension libraries and the Envoy output. It does not scan nixpkgs store paths.
   - A file passes only when an allowlist entry names the Nix derivation that built it from source, for example `xattr.node` and `dd_pprof.node` in Storage.
   - The check must fail today, because of `bare-*`, `libpg-query.wasm` and `mappings.wasm`. Each later part removes its own findings.
   - Until those parts land, keep the check out of the `verify` CI phase. Use a `TODO(FIRE-047)` comment, so the current CI stays green.

## Acceptance

- The doc exists, and the epic links to it.
- The check fails on today's tree and lists exactly the known findings. It passes on a tree where they are removed; prove this with a temporary allowlist edit, then revert it.
- `nix flake check --no-build --all-systems` passes.

## Work log

### 2026-10-23

Wrote the policy doc, the scanner and the check. Excluded the check from the verify phase with TODO(FIRE-047). The build of the app outputs is in progress.

### 2026-10-23

Files: docs/nix-pinning-policy.md (new), nix/flake-modules/supabase-pinning.nix (new), bin/nix/firestream/supabase/scan-binaries.py (new), flake.nix (import), bin/nix/firestream/ci/profile.nix (excludedChecks entry with TODO(FIRE-047)). The epic links to the doc.

Verification: the scan of today's outputs fails with 42 findings and no stale entries. The findings are the bare-fs, bare-os and bare-url prebuilds (ELF, Mach-O, PE), source-map mappings.wasm in Storage, and libpg-query.wasm in postgres-meta and Studio. A temporary allowlist edit made the check pass (103 binaries allowed). The edit is reverted. nix flake check --no-build --all-systems passes. make ci-dry-run lists verify without the check.

Deviation: the Envoy target does not build today. The envoy-1.36.2-deps FOD hash drifted (specified J1TL/F9n..., got /1D2zO4P...). The check keeps the Envoy target, so a full build of the check stops at that dependency. The proofs above ran with the Envoy target removed in a temporary edit. FIRE-044 owns the Envoy fix.
