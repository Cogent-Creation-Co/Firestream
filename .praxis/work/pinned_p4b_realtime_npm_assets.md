---
id: FIRE-048
title: "P4b: Realtime npm assets through pnpm2nix, with no binary fetched"
type: feature
status: done
priority: medium
tags: [beam, nix, node, supabase]
affects: []
depends-on: [FIRE-045, FIRE-046]
---

# P4b: Realtime npm assets through pnpm2nix, with no binary fetched

Epic: [[FIRE-041]]. Found in [[FIRE-046]].

## Problem

The Realtime assets hook in `bin/nix/firestream/beam/mkMixRelease.nix` uses one `fetchNpmDeps` FOD for `assets/`. That is a monolithic package-manager FOD. Its lockfile also has the `oxlint` and `oxfmt` npm binary packages as devDependencies. The build does not run them, and the release does not ship them, but the FOD downloads them. The policy (`docs/nix-pinning-policy.md`) forbids both.

## Scope

- Install the assets npm deps with the vendored pnpm2nix (`bin/nix/firestream/node/pnpm2nix/`): a `pnpm import` lockfile, the JSON lock, and the platform filter that stops npm binary packages. Use pnpm2nix's existing interface.
- Remove the dev-only binary packages from the install, or filter them out. Build-time tools come from nixpkgs (esbuild) or from the source-built Tailwind ([[FIRE-046]]).
- Remove the `fetchNpmDeps` path from `mkMixRelease`, or keep it only behind an explicit opt-in that no Supabase image uses.

## Acceptance

- No `fetchNpmDeps` FOD remains in the Realtime build, and no npm binary package is fetched.
- `checks.firestream-pnpm-lock-json` covers the new lockfile.
- Realtime builds, `/assets/app.css` returns 200, and the websocket change-event test passes.
- `firestream-beam-container` and `nix flake check --no-build --all-systems` pass.

## Studio: unused prebuilt WASM in the build tree

Found in [[FIRE-045]]. The Studio build tree still holds prebuilt `.wasm` files from npm packages: next resvg, shiki onig, nitro, source-map and libpg-query 15.2.0. The build does not run them, and the image does not ship them, but they are fetched.

- For each one, prove that the build never loads it (a trace), then delete it before `next build`. If the build does load one, build it from its GitHub source at a commit SHA.
- Extend the pre-build `find-machine-code.cjs` check to fail on any WASM or ELF file in `node_modules` that is not on its allowlist.
- pnpm2nix now has `excludePackages` ([[FIRE-045]]). Use it where a whole package can go.

Provenance note: the npm gitHead `7a7825a4` of libpg-query 17.6.0 is not on GitHub. [[FIRE-045]] used the branch head that npm published three minutes later. The build output passes the upstream tests (41/41) and gives the same parse trees as npm on 50 statements. Record this in `docs/supabase-maintainer.md`.

## Work log

### 2026-10-23

Milestone: Realtime assets through pnpm2nix.

- New lockfile `src/containers/firestream/supabase-realtime/assets/pnpm-lock.{yaml,json}`. It is `pnpm import` (pnpm 10.26.1, auto-install-peers off) of the upstream assets/package-lock.json. The integrity values equal the npm lock.
- module.nix builds the assets node_modules with pnpm2nix. It excludes oxlint, oxfmt and tinypool, and installs with `--prod`. The platform `none` drops every @oxlint/binding-* and @oxfmt/binding-* package.
- The build fetches three npm tarballs only: @supabase/realtime-js 2.114.0, @supabase/phoenix 0.4.5 and tslib 2.8.1.
- mkMixRelease: `assets.nodeModules` replaces `assets.npmDepsHash`. The fetchNpmDeps FOD and npmConfigHook are gone. An old caller gets a clear error. The tree is a disallowed reference of the release.
- `pnpm-lock-json.sh` and the check `firestream-pnpm-lock-json` now read lockfiles two levels deep.

Verification:
- The release digests equal the last FIRE-046 release: app-ea59ac69 (JS) and app-77826b6b (CSS).
- The scanner passes for the release: 25 binaries allowed, 0 unallowed. The closure holds no npm tree.
- `firestream-pnpm-lock-json` passes. A stale assets JSON fails it.
- Docker: `/healthcheck` 200, `/assets/app.css` 200 (52455 bytes), `/assets/app.js` 200. A websocket client joins `realtime:p2` and gets `Subscribed to PostgreSQL`. After an insert it gets `postgres_changes INSERT todos {'id': 1, 'task': 'from-fire-048'}`. The log has no `[error]` line.

### 2026-10-23

Studio milestone, and the ticket is done.

Trace (strace openat and execve, plus a `--require` hook on WebAssembly compile, instantiate, validate and Module):
- The install puts 9 WASM files in node_modules. They are libpg-query 15.2.0 and 17.6.0, next 16.3.6 (@mswjs llhttp, @vercel/og resvg and yoga, source-map08), nitro source-map, shiki 3.13.0 onig, and source-map 0.7.6.
- Every compile is llhttp, 48643 bytes. That copy is base64 inside Node.js undici and inside the Next edge-runtime undici.
- The build opens the @mswjs llhttp.wasm (61045 bytes) but never compiles it. No output holds it.
- The build opens libpg-query 17.6.0 only after the source build replaces it.

Studio changes (`package.nix`):
- `unusedWasm` names the 9 files. preBuild deletes them before `next build`. A name that matches no file stops the build.
- `excludePackages` adds 8 packages that hold only WASM and that no Studio component installs: @electric-sql/pglite, @parcel/watcher-wasm, @resvg/resvg-wasm, @vercel/og, markdown-wasm, unwasm, xxhash-wasm, yoga-wasm-web.
- The output equals the old output, except the random build ID and the per-build keys (15674 files equal, 198 differ by the build ID only, 5 manifests differ by the keys only).

The check `find-machine-code.cjs`:
- It finds WASM files now. It takes `--allow REGEX` and `--native-only`.
- Studio runs it on node_modules for all kinds. It runs it on the pnpm store with `--native-only`, because store file names are hashes.
- A test on the Studio output finds the WASM file and exits 1. `--allow` and `--native-only` exit 0.

Files (git-added):
- `bin/nix/firestream/beam/mkMixRelease.nix`
- `bin/nix/firestream/node/native/find-machine-code.cjs`
- `bin/nix/firestream/node/pnpm-lock-json.sh`
- `nix/flake-modules/checks.nix`
- `src/containers/firestream/supabase-realtime/module.nix`
- `src/containers/firestream/supabase-realtime/assets/pnpm-lock.{yaml,json}`
- `src/containers/firestream/supabase-studio/package.nix`

File changed, not staged (it holds FIRE-044 edits): `docs/supabase-maintainer.md`. It has new pin rows and a provenance section with the libpg-query 17.6.0 note.

Verification:
- The scanner ran with the real check config on Studio and Realtime: 26 binaries allowed, 0 unallowed.
- `firestream-pnpm-lock-json`, `firestream-beam-container` and `supabase-realtime-mix-deps` pass.
- `nix flake check --no-build --all-systems` passes.
- Docker Studio test: `/api/platform/profile` 200. The pg-meta table list through Studio shows `public.fire048_probe`. `/project/default/editor` 200. `/api/parse-query` 200.
- Docker Realtime: see the entry above.

Deviations:
- The pnpm store still receives the deleted WASM files, because they are in packages that the build installs. Nothing in node_modules holds them during `next build`.
- I did not run the full `supabase-no-prebuilt-binaries` build. It waits for the Envoy build (FIRE-044).
- The loaded Realtime and Studio images replace the local tags. Containers, volumes and the network are gone.

Open item: the usage comment of mkMixRelease in `bin/nix/firestream/default.nix` still shows `mixDepsHash`. That file holds other unstaged edits.
