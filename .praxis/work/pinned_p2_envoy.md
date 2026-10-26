---
id: FIRE-044
title: "P2: Envoy 1.39.1 with per-dependency pins and an offline Bazel build"
type: feature
status: done
priority: high
tags: [envoy, nix, supabase]
affects: []
depends-on: [FIRE-042, FIRE-043]
---

# P2: Envoy 1.39.1 with per-dependency pins and an offline Bazel build

Epic: [[FIRE-041]]. Spike: [[FIRE-040]].

A new package, `bin/nix/firestream/packages/envoy/`, replaces the `pkgs.envoy` override in `src/containers/firestream/supabase-envoy/module.nix:40-42`.

## Facts from research

- Envoy v1.39.1 uses WORKSPACE, not bzlmod: `.bazelrc` has `--noenable_bzlmod --enable_workspace`. `.bazelversion` is 8.7.0, and the pin has `bazel_8` 8.4.2.
- `bazel/repository_locations.bzl` has 115 `http_archive` entries and `api/bazel/repository_locations.bzl` has 13, each with a URL and a sha256. About 93 come from GitHub.
- The Rust crates: `Cargo.lock` (208 packages) and `Cargo.Bazel.lock`, with a sha256 for each crate. nixpkgs sets `CARGO_BAZEL_REPIN=true`; that is a drift source.
- The Go modules: about 15 `go_repository` entries with go.sum `h1:` hashes. They do not use the repository cache.
- `pip_parse` installs about 1,500 hashed requirements at fetch time. Many of them are wheels.
- Prebuilt toolchains that Bazel would fetch: LLVM 18 with sysroots and libc++, Python 3.12.3, Go 1.24.6, Rust 1.88, protoc zips, emsdk, buf, jq, yq, shellcheck, the JDK.
- No Nix build of Envoy with per-dependency pins exists yet. Prior art: the old nixpkgs `bazel_5`/`bazel_6` `src-deps.json` distDir, and the nixpkgs envoy patches 0001, 0003, 0004 and 0005 (0005 from nixpkgs master commit `eaf3faa714`).

## Scope

1. **Bazel 8.7.0** from the GitHub release `-dist.zip`, a source archive. Override nixpkgs `bazel_8`.
2. **The source:** `fetchFromGitHub envoyproxy/envoy` at the v1.39.1 commit.
3. **`generate-deps.py`.** It runs outside Nix, and its output `deps.json` is committed.
   - It reads both `repository_locations.bzl` files and `Cargo.Bazel.lock`.
   - It writes one `fetchurl { url; sha256; }` for each archive that the build needs.
   - It leaves out the prebuilt entries: protoc zips, FIPS, LLVM, sysroots, libc++, the Python interpreter, the Go SDK.
   - A check regenerates the file and compares it.
4. **Offline Bazel.**
   - Build a `linkFarm` repository cache at `content_addressable/sha256/<hex>/file`.
   - Run Bazel with `--repository_cache=<farm> --repository_disable_download --repo_env=BAZEL_HTTP_RULES_URLS_AS_DEFAULT_CANONICAL_ID=0`.
   - Use no `fetchAttrs` and no deps FOD.
5. **Nix toolchains instead of the prebuilt ones.**
   - Rebase the nixpkgs envoy patches onto 1.39.1: system Python, system C/C++, the Rust wiring and the Go SDK pin.
   - Go: `go_version = "host"`. protoc: nixpkgs `protobuf`.
   - Do not use `--config=clang`.
6. **The repositories outside the cache.**
   - crate_universe: no repin; use the checked-in lock.
   - Go modules: a Nix-built module proxy with `GOPROXY=file://`.
   - `pip_parse`: stub it out if `//source/exe:envoy-static` does not need it. If it does, build a wheelhouse from sdists only.
7. **WASM:** use `--define=wasm=disabled`, after you prove that the Supabase Envoy config uses only Lua filters.
8. **Method:** run `bazel build --nobuild` offline until no fetch is missing, then run the full build. Use `systemd-run` with a memory limit. Expect 1–2 hours on 16 or more cores.
9. **Image:** point `supabase-envoy` at the new package, keep the build-time `envoy --mode validate`, delete the `depsHash` override, and set `options.nix` to version 1.39.1.

## Acceptance

- `nix build .#supabase-envoy` succeeds, and so does a rebuild after you delete its store output. No fixed-output derivation except the per-file `fetchurl`s and the Go modules.
- `envoy --version` reports 1.39.1, and the rendered config passes `--mode validate`.
- The `deps.json` check passes, and a stale file fails it.
- The P0 scan passes for Envoy.
- The FIRE-021 gateway acceptance passes in Docker: the key checks, the `sb_` keys, the routes and the Realtime websocket.

## Work log

### 2026-10-23

Start. Envoy v1.39.1 is commit `b579d07d3ad7ee11d32b105e91a5a39ad24718d7`. Bazel 8.7.0 `-dist.zip` hash is `sha256-bm+gkkrAVepJWZddDEV1+jX4jRdD+tZ1TH2YG9adIyk=`. Patch 0004 is obsolete, because 1.39.1 pins rules_rust 0.69.0.

WASM evidence: the pinned Supabase config (supabase@bc27d26, `docker/volumes/api/envoy`) uses these extensions only: http_connection_manager, lua (x7), rbac, cors, basic_auth, router, stdout access log and the downstream connections monitor. It has no wasm and no dynamic module reference. So `--define=wasm=disabled` is safe.

Note: the root tmpfs `/tmp` is full (4 GB). Scratch work goes to `~/.cache/fire-044`.

### 2026-10-23

Generator done. `generate-deps.py` writes `deps.json`: 281 archives, 9 Go modules and the Rust 1.88.0 channel components. It parses nested rule calls with a string-aware match, and `--previous` keeps it offline for the check.

Offline `bazel build --nobuild //source/exe:envoy-static` is clean, with downloads disabled. These changes made it clean:
- `bazel_features` from repository_locations is in the needed set.
- The patch drops the `buf` toolchain registration. Toolchain resolution fetched the prebuilt `buf`.
- cel-cpp runs nixpkgs `antlr4_13` through an `sh_binary`. No Java rule and no ANTLR jar remain.
- `BAZEL_LLVM_PATH` is a join of nixpkgs LLVM 21, clang and libclang. The Rust SDK runs bindgen with it, and the builtin modules run `llvm-objcopy` from it.
- The patch drops the Debian sysroot data dependency of the Rust SDK build script.
- fenix `fromManifest` reads the Rust components from `deps.json`. `toolchainOf` with a hash would be IFD.

The output base fetched only local repositories and toolchain metadata proxies. It fetched no JDK, sysroot, LLVM, Go SDK, protoc or buf archive. Both systems evaluate the package drvPath.

### 2026-10-23

Full offline compile done in scratch (`~/.cache/fire-044`), in the derivation build env (`nix develop -i`). `envoy-static --version` reports `b579d07…/1.39.1/Distribution/RELEASE/BoringSSL`.

- Time: about 53 minutes over two runs. The 28 GB cap killed the first run at 24 jobs; the second run used 12 jobs.
- Fixes during the compile: `--action_env=PATH` and `--host_action_env=PATH`, because the strict action PATH has no bash, compiler or Go. Also `--action_env=CMAKE_POLICY_VERSION_MINIMUM=3.5` for libevent under CMake 4.
- Added `checks.<linux>.supabase-envoy-deps`. It passes for the committed file and fails for a file with one archive removed.
- The image module uses `firestream.packages.envoy`. The `depsHash` override is gone, and the version is 1.39.1.

Next: the Nix build, the image, the rebuild proof and the gateway acceptance.

### 2026-10-23

Nix build and image done.
- `nix build .#supabase-envoy` succeeds. Envoy takes 58 minutes in the sandbox at 12 jobs (Bazel 3,410 s, 10,153 actions). The output is 123 MB and refers only to glibc.
- `envoy --version`: `b579d07…/1.39.1/Distribution/RELEASE/BoringSSL`. The image build runs `envoy --mode validate` on the rendered config: `OK`.
- Sandbox fixes: the sandbox has no `/usr/bin/env` and no `/bin/bash`. The build fetches with `--nobuild`, rewrites those shebangs in the fetched repositories, fetches again, and then builds. The source tree gets the same rewrite.
- The only fixed-output derivations of the package are the per-file `fetchurl`s, the Go module files, the fenix components, the cargo-bazel crates and the two GitHub source archives (Envoy, Bazel dist). No monolithic deps FOD remains.

Docker acceptance (db, rest, auth, realtime and the new gateway, compose project `fire044`):
- `/rest/v1/<table>`: anon 200, service 200, no key 401, bad key 401.
- `sb_` keys: `sb_publishable_*` 200 and `sb_secret_*` 200, with rows.
- `/auth/v1/health` with a key: 200 (GoTrue v2.197.0). Signup with the publishable key: 200. `/graphql/v1`: 200.
- Realtime websocket with the anon key: joined, subscribed, and an insert arrived as `postgres_changes`.
- Dashboard: no credentials 401, wrong 401, right 503 (Studio was not started).
- Gap outside this ticket: with the publishable key, Realtime rejects the websocket (`error_generating_signer`). The gateway substitutes the ES256 anon key, and the Realtime service has no JWKS. PostgREST has the JWKS and accepts it.

### 2026-10-23

Acceptance done.

- Rebuild proof: I deleted the Envoy output, its referrers and my image tarball, then rebuilt `.#supabase-envoy`. The rebuild has the same NAR hash as the first build: `sha256-azFf29Dm…`, 123,078,432 bytes. It took 2 h 12 min because other builds loaded the machine.
- The P0 scanner on the Envoy output finds one binary, `bin/envoy`. The existing `allowAll` entry covers it. `checks.x86_64-linux.supabase-no-prebuilt-binaries` depends on the new derivation.
- `nix flake check --no-build --all-systems` passes. `supabase-envoy-deps` passes, and a stale file fails it.

Files:
- `bin/nix/firestream/packages/envoy/`: `default.nix`, `deps.json`, `generate-deps.py`, `bazel.nix`, `cargo-bazel.nix`, `cargo-bazel.Cargo.lock`, `bazel-nix.BUILD.bazel`, and `patches/{envoy-nix-toolchains,cel-cpp-antlr}.patch`.
- `bin/nix/firestream/{default.nix,packages/default.nix}` pass `fenix` and `system`, and expose `packages.envoy`.
- `src/containers/firestream/supabase-envoy/{module,options}.nix`: the new package, version 1.39.1, no `depsHash`.
- `nix/flake-modules/containers/supabase-envoy.nix`: `checks.supabase-envoy-deps`.
- `docs/supabase-maintainer.md`: the build cost and the regeneration steps.

Deviations:
- Rust 1.88.0 comes from fenix `fromManifest`, with the component hashes in `deps.json`. The lock digest needs that exact rustc, and `toolchainOf` would need IFD.
- `pip_parse` stays. It creates only the hub repositories, and the build installs no wheel.
- ANTLR comes from nixpkgs `antlr4_13` 4.13.2. cel-cpp pins the 4.13.1 runtime.
- LLVM 21 from nixpkgs supplies libclang and llvm-objcopy for the Rust dynamic modules. The C++ compiler is GCC.

Open: Realtime has no JWKS, so it rejects the ES256 token that replaces a publishable key on the websocket.
