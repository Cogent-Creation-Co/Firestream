---
id: FIRE-050
title: "S1: one canonical container list for the SBOM and the CI build phase"
type: feature
status: backlog
priority: high
tags: [sbom, nix, ci]
affects: []
depends-on: []
---

# S1: one canonical container list for the SBOM and the CI build phase

Epic: [[FIRE-049]]

## Scope

1. Add `bin/nix/firestream/lib/fleet.nix` with `canonicalContainers reg`.
   - The function groups the `config.firestreamContainers` entries by `dockerImage.drvPath`. This finds the aliases with no hand-written list.
   - It keeps the shortest name in each group.
   - Export it as `firestreamLib.fleet` through `bin/nix/firestream/default.nix`.
2. In `nix/flake-modules/aggregate.nix`, replace the `wanted` list with `canonicalContainers`. Remove the stale "Phase 3" comments.
3. In `nix/flake-modules/ci-profile.nix`, pass `containerNames` through `specialArgs`, the same way `checkNames` goes through (line 64).
4. In `bin/nix/firestream/ci/profile.nix`, build `containerPackages` from `containerNames`, and delete the wrong alias comment.
5. Make sure a `packages.<sys>.<n>` attr exists for every canonical name, because `pkgAttr` reads that path.

## Notes

- `build` then also builds `nginx`, `odoo-15/16/17`, `postgresql-16` and `redis-7`. `attest` builds them anyway through `exportReferencesGraph`.
- All 30 registry entries already carry `.module.metadata`, so `collectArtifacts` accepts each one.

## Acceptance

- The canonical list has 27 names, including `nginx`.
- `make ci-dry-run` lists all 27 under `build`.
- `nix build .#manifest` includes all 27 images.
