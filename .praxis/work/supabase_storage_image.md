---
id: FIRE-023
title: "Storage API image"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: [FIRE-013, FIRE-018]
---

# Storage API image

Epic: [[FIRE-009]]

## Goal

Build `supabase/storage` v1.81.0 with Nix and package it as a firestream
image that uses SeaweedFS as its S3 backend.

## Scope

- Build with `firestream.mkNodePackage`
  (`bin/nix/firestream/node/mkNodePackage.nix`) and wrap it with
  `mkNodeContainerModule`. Follow the nextjs container.
- Add `src/containers/firestream/supabase-storage/{options.nix,module.nix}`,
  the flake-module, the `flake.nix` import and the CI profile entry.
- Set the S3 settings and the features to turn off from the spike
  [[FIRE-013]]. Default the endpoint and keys to the SeaweedFS `S3_LOCAL_*`
  values.
- Wire `IMGPROXY_URL` to the imgproxy image ([[FIRE-022]]).
- Take `ANON_KEY`, `SERVICE_KEY` and the JWT secret from [[FIRE-018]].

## Acceptance

- The image builds and `GET /status` answers.
- An upload and a download through Storage land in a SeaweedFS bucket.

## Spike findings

The spike [[FIRE-013]] says "go with changes":

- Build with `nodejs_24`, `engine-strict` off and a writable npm cache.
- Keep `STORAGE_VERSIONING_ENABLED` off. With versioning on, a second TUS upload to an existing key fails with 409. MinIO shows the same failure, so it is a Storage defect.
- This release does not read `GLOBAL_S3_PROTOCOL`. Put the scheme in `GLOBAL_S3_ENDPOINT`.
- Use a dedicated bucket. Map the `S3_LOCAL_*` values to the Storage env names.
- After a TUS upload completes, SeaweedFS reports a HEAD offset of 0, but the object stays intact. Document this and report it upstream.
- `@datadog/pprof` ships prebuilt `.node` files. Follow the project decision on vendored npm binaries.

The working expression and test scripts are in `/tmp/firestream-supabase-spikes/FIRE-013/`.

## Node build decision

Use pnpm2nix (see the decisions in [[FIRE-009]]). Generate a `pnpm-lock.yaml` from the upstream `package-lock.json` with `pnpm import`. Build `@datadog/pprof` and any other native module from source, or remove it if it is optional. Check first that pnpm2nix supports the lockfile format and the pnpm version.

## Work log

### 2026-10-22

Image done: `packages.supabase-storage` builds `firestream-supabase-storage:1.81.0` (container name `storage`).

Build route: pnpm2nix (the patched `pnpm2nix` input; see FIRE-024). `pnpm-lock.yaml` is `pnpm import` (pnpm 10.26.1) of the upstream package-lock.json, with one edit: `pnpm import` resolved the `github:supabase/pg-boss#feat/exactly_once` specifier again and took the branch head 67094d7c. The lockfile now points at 89b675b0, the commit that package-lock.json pins. The two commits have the same dependencies and no build step. Node 24. Hoisted install (`shamefully-hoist`), because Storage imports packages that it does not declare.

Built from source:
- esbuild 0.28.1 (Go) for build.js, through ESBUILD_BINARY_PATH. The prebuilt @esbuild packages are deleted before the build.
- `@datadog/pprof` 5.15.1: node-gyp from the DataDog/pprof-nodejs commit that the npm tarball names (110d171c). The npm tarball has no C++ source.
- `fs-xattr` 0.4.0: node-gyp from the npm tarball.
- A build check fails on any other `.node` file.
- Defect found: the nixpkgs node-gyp wrapper exports npm_config_nodedir for Node 22, and that wins over --nodedir. The first pprof build failed dlopen in Node 24. Both Node packages now use the node-gyp of their own Node.js.

Files: src/containers/firestream/supabase-storage/{options.nix,module.nix,package.nix,pnpm-lock.yaml}, nix/flake-modules/containers/supabase-storage.nix, its flake.nix import, `supabase-storage` in containerPackages.

Verification (docker, `--read-only --tmpfs /tmp`, with `.#supabase-postgres`, `.#seaweedfs` with S3 auth (identity firestream/firestream-secret) and the imgproxy image):
- `GET /status` 200. healthd `/readyz` 200. Log shows appVersion 1.81.0.
- Storage ran its migrations on `supabase_storage_admin`.
- Create bucket 200. Upload of 300000 random bytes 200. Download sha256 equals the source.
- SeaweedFS holds the object at `/buckets/supabase-storage/stub/fire-node-bucket/dir/blob.bin/<version>` (300000 bytes).
- `GET /render/image/authenticated/...?width=100&height=75` returns a 100x75 PNG through imgproxy.
- pprof, fs-xattr load in the image.

Closure of the app: 442 MiB (the spike build had 1.2 GiB).

Facts for Phase 3 (FIRE-028, FIRE-029):
- The bucket `GLOBAL_S3_BUCKET` (default `supabase-storage`) must exist before Storage starts. Storage does not create it. Create it in the SeaweedFS bucket hook or a compose init step.
- Map S3_LOCAL_*: GLOBAL_S3_ENDPOINT (with the scheme), AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, REGION and STORAGE_S3_REGION. The image defaults equal the chart values.
- Port 5000. Readiness: GET /status. Required env: DATABASE_URL (supabase_storage_admin), ANON_KEY, SERVICE_KEY, AUTH_JWT_SECRET. IMGPROXY_URL defaults to http://imgproxy:5001.
- STORAGE_VERSIONING_ENABLED stays false.
- Known SeaweedFS difference (from FIRE-013): a HEAD after a finished TUS upload returns offset 0. Tell clients to set `removeFingerprintOnSuccess`. Report it upstream.

Open item: the image takes a writable /tmp (tmpfs in the test).

### 2026-10-22

Correction to the open item above: Storage starts and accepts a standard upload with `--read-only` and no tmpfs. TUS uploads were not tested without a writable /tmp.
