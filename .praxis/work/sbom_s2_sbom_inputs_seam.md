---
id: FIRE-051
title: "S2: sbomInputs seam on every builder, and a metadata name for each image"
type: feature
status: backlog
priority: high
tags: [sbom, nix]
affects: []
depends-on: [FIRE-050]
---

# S2: sbomInputs seam on every builder, and a metadata name for each image

Epic: [[FIRE-049]]

## Scope

1. Add `bin/nix/firestream/lib/sbom-inputs.nix`. Each constructor returns this record:
   ```
   { ecosystem; kind = "lockfile" | "scan"; path; pinning = "lock" | "hash"; repoDir = <repo-relative dir or null>; }
   ```
   `repoDir` is not null only when `path` is inside the flake source. To find it, strip `toString inputs.self`.
2. Constructors:
   - `uvLock`, `npmLock`, `pnpmLockJson`.
   - `cargoLock`: the path can be `"${src}/Cargo.lock"` inside a fetched source. The build reads it at build time, so there is no IFD.
   - `mixDeps`: evaluate the committed `mix-deps.nix` with stub `fetchHex`/`fetchFromGitHub`.
   - `envoyDeps`.
   - `goBinaries` and `jarDir`: scan records.
   - `haskellDeps`: walk `getCabalDeps`.
   - `sourcePins`: read `.src` with `owner/repo/rev` or `urls`, and give `pkg:github` or `pkg:generic`.
3. Set `passthru.sbomInputs` on each builder. A builder also forwards the `sbomInputs` of each native sub-build that it consumes.

   | Builder | Records |
   |---|---|
   | `containers/python-workspace.nix` | `uvLock` for the root, `extend`, `replace` and `extraWorkspaces` |
   | `node/mkNodePackage.nix` | `npmLock` or pnpm lock |
   | `node/pnpm2nix/derivation.nix` | `pnpmLockJson` |
   | `beam/mkMixRelease.nix` | `mixDeps` (`passthru.mixDepsFile`), plus the cargo lock of the NIF |
   | `rust/mkRustPackage.nix`, `nix/package.nix` | root `Cargo.lock` |
   | `node/native/{next-swc,lightningcss,tailwindcss-oxide}.nix`, the postgres-meta oxfmt binding | `cargoLock` from `src` |
   | `packages/{seaweedfs-weed,imgproxy,supabase-auth}.nix`, the storage Go part | `goBinaries` |
   | `packages/envoy/default.nix` | `envoyDeps` |
   | kafka and spark `module.nix` | `jarDir`, through a new `extraSbomInputs` argument on `mkContainerModule` |
   | postgrest `module.nix` | `haskellDeps`, through `extraSbomInputs` |

4. `containers/base.nix` collects `extraSbomInputs`, the `sbomInputs` of every entry in `allRuntimeDeps`, and `sourcePins allRuntimeDeps`. It removes duplicates and passes the list to `mkContainerMetadata`.
5. `eval-container.nix` passes the registry key as `metadataName`. `base.nix` uses it for `mkContainerMetadata` (default `name`). This stops the `postgresql`, `odoo` and `realtime` name collisions in `merge-sboms`.

All paths are under `bin/nix/firestream/` unless shown otherwise.

## Acceptance

- Each of the 27 images has a non-empty `sbomInputs` list. Evaluating the lists needs no IFD.
- `metadata.json` of `supabase-postgres` and of `postgresql-17` have different container names.
