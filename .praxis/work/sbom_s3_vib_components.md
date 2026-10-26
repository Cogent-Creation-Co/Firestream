---
id: FIRE-052
title: "S3: firestream-vib makes ecosystem components, nix licenses and the MIT top-level license"
type: feature
status: backlog
priority: high
tags: [sbom, rust]
affects: []
depends-on: [FIRE-051]
---

# S3: firestream-vib makes ecosystem components, nix licenses and the MIT top-level license

Epic: [[FIRE-049]]

## Scope

All Rust changes are in `src/lib/rust/firestream-vib`, unless shown otherwise.

1. Add the subcommand `collect-components --inputs <json> --output <components.json>`. Put one parser module for each format under `src/components/`:

   | Input | Purl | Scope rule |
   |---|---|---|
   | uv.lock | `pkg:pypi` | walk from the workspace members, skip dev groups |
   | package-lock.json | `pkg:npm` | skip `dev: true` |
   | pnpm-lock.json v9 | `pkg:npm` | walk prod `importers` to `snapshots`; use the same platform filter as `pnpm2nix/lockfile.nix` |
   | Cargo.lock | `pkg:cargo`, or `pkg:github` for git sources | — |
   | mix-deps JSON | `pkg:hex` / `pkg:github` | — |
   | envoy `deps.json` | `pkg:cargo` / `pkg:golang` / `pkg:github` / `pkg:generic` | — |
   | `go version -m` output, made by the Nix wrapper | `pkg:golang` | — |
   | jar `META-INF/maven/**/pom.properties` | `pkg:maven` | use the jar file name as a fallback; add the `zip` crate |
   | haskell and source-pin JSON | `pkg:hackage` / `pkg:github` / `pkg:generic` | — |

   Each component has the property `firestream:source`, which holds the lockfile path or the scanned derivation.
2. `generate-metadata` gets two new options:
   - `--extra-components <file>`: adds the components to the CycloneDX `components` and `dependencies`, and to the SPDX packages.
   - `--nix-package-info <file>`: a map from store path to `{pname, version, spdxLicense, homepage}`, built at eval time from `packageList`. Move this logic out of `buildFleetSourceMap` in `bin/nix/firestream/lib/manifest.nix` so both places share it. It replaces the version regex where data exists, and it fills in licenses instead of `NOASSERTION`.
3. Add `licenseId` to `MetadataConfig` in `src/lib/rust/firestream-metadata/src/spec.rs`. The default is `MIT`, to match `LICENSE`. Remove the Apache-2.0 strings at `closure_graph.rs:441,488`.
4. Add the component count for each ecosystem to `metadata.json`.
5. `bin/nix/firestream/lib/metadata.nix`: `mkContainerMetadata` takes `sbomInputs` and `packageInfo`. In the same derivation, it runs `collect-components` (with `go` from nixpkgs for the Go scan) and then `generate-metadata`.

`merge-sboms` needs no change. It already keeps any purl type.

## Acceptance

- `cargo test -p firestream-vib` passes, with a fixture test for each parser under `tests/fixtures/`.
- `nix build .#legacyPackages.x86_64-linux.sbom.supabase-studio` gives `pkg:npm`, `pkg:cargo` and `pkg:github` components (count them with `jq`).
- `make manifest-validate` passes.
- `grype sbom:_build/manifest/sbom-cyclonedx.json` reports matches in the language ecosystems.
