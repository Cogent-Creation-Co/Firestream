---
id: FIRE-045
title: "P3: Node images without prebuilt binaries"
type: feature
status: done
priority: high
tags: [nix, node, supabase]
affects: []
depends-on: [FIRE-042, FIRE-043]
---

# P3: Node images without prebuilt binaries

Epic: [[FIRE-041]]

## Scope

1. **Storage** (`src/containers/firestream/supabase-storage/package.nix`):
   - Delete the `bare-fs`, `bare-os`, `bare-url` and `bare-path` prebuilds. They are optional `tar-fs` deps for the Bare runtime, not for Node. Prove it with a Storage upload and download round trip.
   - For `source-map` `mappings.wasm`: rebuild it from its Rust source (wasm32), or delete it if a test proves that the runtime never loads it.
2. **`libpg-query.wasm`** (postgres-meta and Studio): build it from the `pganalyze/libpg_query` and `libpg-query-node` GitHub sources with nixpkgs emscripten. Put it in place after the install.
3. **Studio build tools** (`src/containers/firestream/supabase-studio/package.nix`):
   - Find which native packages `next build` for `apps/studio` actually loads: `@next/swc`, lightningcss, `@tailwindcss/oxide`, oxc, rolldown, sharp.
   - Build each needed one from its GitHub source at a commit SHA: Rust for most, and node-gyp with nixpkgs vips for sharp.
   - Remove the unneeded ones from the install: `@sentry/cli`, `@supabase/cli`, `@ngrok/ngrok`, `@typescript/typescript-linux-x64`.
   - The build must run with no prebuilt native package in the pnpm store.
4. **Output checks:** replace the existing `.node`-only checks with the P0 magic-byte scan ([[FIRE-042]]).

## Acceptance

- The P0 scan passes for postgres-meta, Storage and Studio, with no allowlist entry except the source-built artifacts.
- Docker acceptance passes:
  - postgres-meta `/tables` and typegen;
  - a Storage round trip into SeaweedFS, plus an imgproxy resize;
  - Studio `/api/platform/profile`, and the table list through pg-meta.
- The pnpm lock JSON check and `nix flake check --no-build --all-systems` pass.

## Work log

### 2026-10-23

Storage milestone (Nix build done; Docker test pending).

- The bare-fs, bare-os and bare-url `prebuilds/` directories go. tar-fs maps bare-* through `imports` keys with no `#`. Node.js ignores such keys, so Node never loads them.
- `source-map@0.7.6` `lib/mappings.wasm` is rebuilt from mozilla/source-map 801be934 (tag 0.7.6), `wasm-mappings/source-map-mappings-wasm-api`. New file: `bin/nix/firestream/node/native/source-map-mappings/{default.nix,Cargo.lock}`.
- Toolchain: fenix Rust 1.65.0 with the wasm32 rust-std. A current Rust (1.92) gives different results on tied mappings, because `generated_location_for` keeps the first `binary_search_by` hit. With 1.65, 533548 lookups over all Storage source maps equal the npm file.
- The build-time `.node` check is gone. The P0 scan replaces it. The scan passes for Storage with three entries: xattr, dd_pprof, mappings.wasm.
- Storage now takes `fenix` through extraFactoryArgs.
- Build time: 8 min.

### 2026-10-23

libpg-query milestone (postgres-meta Nix build done; Docker test pending).

- New file `bin/nix/firestream/node/native/libpg-query.nix`. It builds `libpg-query.js` and `libpg-query.wasm` together with nixpkgs Emscripten 4.0.12.
- Sources: pganalyze/libpg_query 1c1a32ed (tag 17-6.1.0) and constructive-io/libpg-query-node 727c0826 (`versions/17`).
- Deviation: the npm gitHead of 17.6.0 (7a7825a4) is not on GitHub. 727c0826 is the head of branch fix/increase-memory. npm published 17.6.0 three minutes after it.
- The Makefile must link without `-j`: the `.js` and `.wasm` targets share one recipe.
- Tests: the upstream v17 suite passes 41/41 with the npm files and with the rebuilt files. 50 statements give equal parse trees.
- postgres-meta replaces both files after the prod install. Its `.node` check is gone. The scan passes with three entries.
- Build times: libpg-query 3.7 min; postgres-meta 3 min.

Studio trace (strace execve plus a `--require` hook on dlopen, child_process and WebAssembly): `next build` loads only `@next/swc-linux-x64-gnu@16.3.6`, `@tailwindcss/oxide-linux-x64-gnu@4.2.4` and `lightningcss-linux-x64-gnu@1.32.0`. It runs no other binary. The only wasm it compiles is llhttp inside Node.js and inside Next's bundled undici. Studio bundles the npm libpg-query loader into a server chunk, so Studio must swap the files before `next build`.

### 2026-10-23

Studio build tools milestone: the three native modules build from source.

- `bin/nix/firestream/node/native/next-swc.nix`: vercel/next.js a758ffcf (tag v16.3.6), `-p next-napi-bindings --features image-extended,tracing/release_max_level_trace`, fenix nightly-2026-07-29 (the rust-toolchain.toml pin). Fixes: an empty `README.md` at the vendor root for the include_dir git crate; build.rs drops the vergen git step, and VERGEN_GIT_SHA/VERGEN_GIT_DIRTY come from the env. Build: about 3 min. 97.5 MB (npm 96.7 MB). A Node.js `transform` call works.
- `tailwindcss-oxide.nix`: tailwindlabs/tailwindcss 69ad7cc5 (tag v4.2.4), `-p tailwind-oxide`, Rust 1.85.0. 3.0 MB (npm 3.1 MB).
- `lightningcss.nix`: parcel-bundler/lightningcss 7f8a861b (tag v1.32.0), `-p lightningcss_node`, Rust 1.92.0. 9.9 MB (npm 10.0 MB).
- All three use cargoHash and `stripAllList = [ "." ]`. Oxide and lightningcss together build in about 2 min.
- pnpm2nix takes a new optional argument `excludePackages` (package names). An excluded package goes like a package of another platform. It is backward compatible: with the argument unset, the drvPaths of Storage, postgres-meta and Studio are equal to those of the old copy.
- Studio excludes every lockfile package with an `os` or `cpu` field (267 names). preBuild runs `find-machine-code.cjs` on node_modules and on the pnpm store, and then installs the three modules and the libpg-query files.

### 2026-10-23

Ticket done.

Correction to the libpg-query entry: Studio does not bundle the libpg-query loader. Turbopack loads it as an external module. Studio still swaps the files before `next build`, because the output file trace copies them into the standalone tree.

Files added (git-added):
- `bin/nix/firestream/node/native/source-map-mappings/{default.nix,Cargo.lock}`
- `bin/nix/firestream/node/native/libpg-query.nix`
- `bin/nix/firestream/node/native/{next-swc,tailwindcss-oxide,lightningcss}.nix`
- `bin/nix/firestream/node/native/find-machine-code.cjs`

Files changed:
- The package.nix of Storage, postgres-meta and Studio.
- The module.nix of Storage and Studio, which now take `fenix`.
- The flake-modules supabase-storage.nix and supabase-studio.nix, which pass `fenix`.
- pnpm2nix (`excludePackages`).
- `nix/flake-modules/supabase-pinning.nix`: three new allow entries. This file is shared, so it stays unstaged.

Verification:
- The scanner ran with the real check config for the three images: 7 binaries allowed, 0 unallowed, 0 stale.
- `checks.firestream-pnpm-lock-json` passes. `nix flake check --no-build --all-systems` passes.
- Docker (supabase-postgres, seaweedfs, imgproxy, postgres-meta, Storage, Studio):
  - pg-meta `/tables` lists `public.fire_node_probe`. Typegen returns 200. `/query/parse` returns a parse tree.
  - Storage: bucket 200 and upload 200. The download sha256 equals the source. SeaweedFS holds 300000 bytes. A render returns a 100x75 PNG.
  - Studio: `/api/platform/profile` 200. pg-meta tables through Studio list the probe table. `/project/default/editor` 200. `/api/parse-query` returns 200.
  - In the image, tar-fs packs and source-map 0.7.6 resolves a mapping.
- Negative test: find-machine-code finds 24 ELF files in the old Studio tree.

Build times (32 cores): Storage 8 min, postgres-meta 3 min, libpg-query 3.7 min, next-swc about 3 min, oxide plus lightningcss about 2 min, Studio 2.3 min. The six images took 8.2 min.

Deviations:
- The libpg-query source is the branch head 727c0826. The npm gitHead is not on GitHub.
- source-map wasm uses Rust 1.65 so that tied lookups match npm.
- The next-swc build.rs loses its vergen git step.
- The Studio build tree still holds WASM files that it does not run: next resvg, shiki onig, nitro and source-map mappings, and libpg-query 15.2.0. None ships.
- Docker images that I loaded stay in the local image store. Containers, volumes and the network are gone.
