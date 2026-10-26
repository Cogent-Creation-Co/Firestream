---
id: FIRE-053
title: "S4: build the Next.js npm lockfiles with importNpmLock, with no npmDepsHash"
type: feature
status: backlog
priority: high
tags: [sbom, nix, node]
affects: []
depends-on: [FIRE-051]
---

# S4: build the Next.js npm lockfiles with importNpmLock, with no npmDepsHash

Epic: [[FIRE-049]]

## Problem

An `npmDepsHash` pins `src/containers/firestream/nextjs/app/package-lock.json`. The three `examples/nextjs-*/app` lockfiles are byte-identical copies, and they use the same hash. A Dependabot bump changes the lockfile but not the hash, so the PR fails the Nix build.

## Scope

1. In `bin/nix/firestream/node/mkNodePackage.nix` (npm path), change the default `npmDepsHash` to `null`.
   - When it is `null` and `src` is a path, use `npmDeps = pkgs.importNpmLock { npmRoot = src; }` and `npmConfigHook = pkgs.importNpmLock.npmConfigHook`. Both exist in the pinned nixpkgs 25.11.
   - An explicit hash keeps the old FOD path. A non-path `src` needs it, because `importNpmLock` would be IFD there.
   - Set `pinning` in the `sbomInputs` record ([[FIRE-051]]) to match the path that the build takes.
2. In `src/containers/firestream/nextjs/module.nix`, delete the default `depHash`. In `options.nix`, change the default of `npmDepsHash` to `null`.
3. In `examples/nextjs-{docker-compose,gke,k3s}/firestream/nextjs-image-overrides.nix`, delete `npmDepsHash` and rewrite the comment. Use grep to find each doc that tells users to paste a hash, and update it.
4. Leave the `pnpmDepsHash` path alone. No in-repo lockfile uses it. The coverage check ([[FIRE-054]]) fails if one ever does.

## Notes

- `importNpmLock` fetches every entry in the lock, including the `@next/swc-*` packages for all platforms. That means more downloads, but `npm_config_optional = "true"` still works.

## Acceptance

- `nix build .#nextjs` succeeds with no `npmDepsHash`.
- The nextjs container in `examples/nextjs-k3s` builds with no `npmDepsHash`.
- A change to the dependencies in `package-lock.json` builds with no hash edit.
