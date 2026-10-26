---
id: FIRE-041
title: "Pinned source builds for Supabase"
type: plan
status: done
priority: high
tags: [epic, nix, supabase]
affects: []
depends-on: [FIRE-042, FIRE-043, FIRE-044, FIRE-045, FIRE-046, FIRE-047, FIRE-048]
---

# Pinned source builds for Supabase

The epic. Its `depends-on` lists every child. Spike: [[FIRE-040]]. Parent work: [[FIRE-009]].

Policy: [`docs/nix-pinning-policy.md`](../../docs/nix-pinning-policy.md). The check `checks.<sys>.supabase-no-prebuilt-binaries` enforces it.

## Goal

Every Supabase image builds from pinned sources. No drifting monolithic FOD remains, and a check proves that no binary ships unless it was built from source.

## The user's rule (2026-10-23)

"Properly pinned" means: build from source, and pin each input to a GitHub link (commit SHA plus hash). Avoid binaries wherever possible.

| Topic | Decision |
|---|---|
| Registry packages (npm, Hex, crates.io, Go proxy) | Allowed as source, pinned by lockfile hash. Remove or rebuild from GitHub source any binary inside them: ELF, `.node`, `.wasm`, prebuilds. This replaces the earlier WASM exception. |
| nixpkgs | Its bootstrap seeds and cache.nixos.org are the trusted base. |
| fenix | Its Rust toolchains are fine. |
| Studio build tools | Build each needed one from source. Drop the ones the build does not need. |
| Envoy | Upstream 1.39.1, built with each dependency pinned on its own. |

## Audit findings (2026-10-23)

- **Binaries that ship:**
  - Storage: the `bare-fs`, `bare-os` and `bare-url` ELF prebuilds, plus `source-map` `mappings.wasm`.
  - postgres-meta and Studio: `libpg-query.wasm`.
- **Binaries that run at build time:**
  - Studio: `@next/swc`, lightningcss, `@tailwindcss/oxide`, oxc, rolldown and sharp.
  - Realtime assets: `tailwindcss_3`, which is a GitHub release binary.
  - Envoy's Bazel deps: the Go SDK, protoc and Node.js.
- **Moving refs:** about ten inputs are pinned by tag, not by commit.
- **Monolithic FODs:** the Envoy Bazel deps (they drift) and `fetchMixDeps` for Realtime and Supavisor.

## Parts

- P0: policy and the binary scan.
- P1: commit-SHA pins.
- P2: Envoy, pinned per dependency.
- P3: Node images without binaries.
- P4: Elixir images.
- P5: verification and CI.

P0 and P1 run first. Then P2, P3 and P4 run in parallel. P5 runs last.

## Work log

### 2026-10-23

Epic done. Every child (FIRE-042 to FIRE-048, and FIRE-058) is done.
- The binary scan gates `verify`.
- The advisory phase `repro-supabase-fods` (mode `e2e`) rebuilds the 3472 Firestream FODs. All of them reproduce.
- Both Supabase e2e suites pass on the source-built images.
