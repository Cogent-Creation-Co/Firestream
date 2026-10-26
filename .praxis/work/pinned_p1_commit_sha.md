---
id: FIRE-043
title: "P1: pin every Supabase input to a commit SHA"
type: feature
status: done
priority: high
tags: [nix, supabase]
affects: []
depends-on: []
---

# P1: pin every Supabase input to a commit SHA

Epic: [[FIRE-041]]

## Scope

- Replace every `tag =` or `rev = "v${version}"` with the commit SHA, and keep the version in a `NOTE:` comment. The inputs:
  - supautils and supabase_vault (`bin/nix/firestream/packages/postgresql-extensions/`);
  - imgproxy (`bin/nix/firestream/packages/imgproxy.nix`);
  - Realtime, Supavisor, postgres-meta and Storage (`src/containers/firestream/*/`);
  - esbuild in Storage;
  - the oxc oxfmt binding in postgres-meta;
  - any other `fetchFromGitHub` in the Supabase builds that uses a tag.
- Go 1.27.3 (`bin/nix/firestream/packages/go-toolchain.nix`): fetch with `fetchFromGitHub golang/go` at the `go1.27.3` commit, not from go.dev.
- Storage's `nan`: fetch with `fetchFromGitHub nodejs/nan` at the commit of the locked version, not from the npm registry.

## Acceptance

- `rg` finds no tag or branch ref in any Supabase `fetchFromGitHub`.
- Each changed source has the same contents as before, except Go and `nan`, which now come from GitHub. Prove this: the NAR hash of the new fetch equals the old one, or the diff of the two trees is empty.
- All ten images build. `nix flake check --no-build --all-systems` passes.

## Work log

### 2026-10-23

Progress. Eleven fetches now use commit SHAs. Nine NAR hashes equal the old hashes. Go builds and reports go1.27.3. The flake check passes. The image builds run now.

### 2026-10-23

Done. Each tag ref is now a commit SHA. ls-remote shows no peeled entries, so every tag is lightweight.

Files: postgresql-extensions/supautils.nix, postgresql-extensions/supabase-vault.nix, packages/imgproxy.nix, packages/go-toolchain.nix, supavisor/module.nix, supabase-realtime/module.nix, postgres-meta/package.nix, supabase-storage/package.nix.

Refs, old to new, with NAR hash equality from nix-prefetch-url --unpack:
- supautils v3.2.2 -> 64792e14. Hash equal.
- supabase_vault v0.3.1 -> 6e0cd916. Hash equal.
- imgproxy v3.31.4 -> 8b0aa782. Hash equal.
- supavisor v2.9.13 -> 5a51fbfa. Hash equal.
- realtime v2.143.3 -> d46540b3. Hash equal.
- postgres-meta v0.100.0 -> 9a2effd1. Hash equal.
- storage v1.81.0 -> f31599e1. Hash equal.
- esbuild v0.28.1 -> bb9db84c. Hash equal.
- oxc oxfmt_v0.66.0 -> 0b4e2e67. Hash equal.
- Go: go.dev tarball -> golang/go 2aae1000 (go1.27.3). The new hash is sha256-9sYPwy...
- nan: npm tarball 2.27.0 -> nodejs/nan 740ef762 (v2.27.0). The new hash is sha256-5Ostel...

Go diff: the file contents are equal. The git tree adds .gitattributes, .gitignore and .github. Nineteen files have a different exec bit. They are helper scripts and test data, and the build does not use them. The tree holds VERSION, so no postPatch is necessary. The built go reports "go version go1.27.3 linux/amd64".

nan diff: every file of the npm tarball is equal. The git tree adds tests, examples, CI files and a Makefile. The .npmignore file excludes them. The pprof-nodejs package-lock.json locks nan 2.27.0. The configurePhase now copies the tree and does not untar.

Envoy: no fixed-output derivation in its closure has one of the changed hashes. I did not build it.

Builds: all nine other images build. The first run failed on npm tarball downloads (a network fault). A second run passed.

nix flake check --no-build --all-systems passes. rg finds no tag or branch ref in the Supabase fetches.

Deviations: postgres-meta and Storage get a NOTE that names the tag, because module.nix sets version. No other deviations.
