---
id: FIRE-054
title: "S5: required SBOM coverage check in verify"
type: feature
status: backlog
priority: high
tags: [sbom, nix, ci]
affects: []
depends-on: [FIRE-050, FIRE-051, FIRE-053]
---

# S5: required SBOM coverage check in verify

Epic: [[FIRE-049]]

## Scope

Add `checks.firestream-sbom-coverage` to `nix/flake-modules/checks.nix`. The `verify` phase picks it up automatically through `checkNames` (`nix/flake-modules/ci-profile.nix:64`), so it is required in `make ci-check` and `make ci`.

The check finds the failures at eval time and builds no image. It fails, and names each offender, when one of these is true:

1. A canonical container ([[FIRE-050]]) is not in the `packages.manifest` artifacts.
2. A package in an image's `packageList` has a vendoring attribute (`cargoDeps`, `npmDeps`, `pnpmDeps`, `goModules`, `mixFodDeps` or `vendorHash`) but has empty `sbomInputs`. For the second level, use the `targets` map of native sub-builds in `nix/flake-modules/supabase-pinning.nix`.
3. A record has a `repoDir` and `pinning = "hash"`, that is, an in-repo lockfile pinned by a hash ([[FIRE-053]]).

`attest` stays advisory.

## Acceptance

- The check passes on the tree.
- Remove `sbomInputs` from one builder: the check fails and names that builder. Restore the builder after.
- Put an `npmDepsHash` back on the canonical Next.js app: the check fails.
