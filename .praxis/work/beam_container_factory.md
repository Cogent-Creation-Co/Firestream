---
id: FIRE-017
title: "Elixir/BEAM container factory"
type: feature
status: done
priority: high
tags: [nix, phase-1, supabase]
affects: []
depends-on: [FIRE-010]
---

# Elixir/BEAM container factory

Epic: [[FIRE-009]]

## Goal

Add a container factory for Elixir releases, beside the Node and Java
factories. Realtime and Supavisor use it.

## Scope

- Add `bin/nix/firestream/containers/beam.nix`, beside `node.nix` and
  `java.nix`. Wire it in `bin/nix/firestream/default.nix`.
- Take a built `mixRelease` package and wrap it with `mkContainerModule`
  (`bin/nix/firestream/containers/base.nix`).
- Use the dependency-fetch method and the NIF approach from [[FIRE-010]].
- Handle the BEAM runtime needs: `RELEASE_COOKIE`, a writable `RELEASE_TMP`
  on a read-only root filesystem, and the node name.
- Add a `beam` runtime type to `eval-container.nix` if the dispatch needs
  one.

## Acceptance

- A minimal Elixir release builds and runs in an image from this factory.
- The image runs with `readOnlyRootFilesystem: true`.
- Existing images do not change (equal drvPaths).

## Spike findings

The spike [[FIRE-010]] says "go". The factory must support these needs:

- A per-app OTP and Elixir set. Realtime uses `erlang_28` with `elixir_1_19`. Supavisor uses `erlang_27` with `elixir_1_18`. Both are in the pin.
- Dependencies through `fetchMixDeps`, one fixed-output derivation for each app.
- A hook for Rust NIFs (rustler) that builds offline from vendored Cargo deps. Supavisor needs it for `pg_query`.
- A hook for npm assets, with esbuild and tailwind from nixpkgs. Realtime needs it; otherwise the build tries a download.
- A pre-start hook to run migrate and seed before the server.
- A smaller closure. `epmd` pulls in 172 MiB of systemd today.

The working expressions are in `/tmp/firestream-supabase-spikes/FIRE-010/`.

## Work log

### 2026-10-22

Done. The Elixir/BEAM builder and the container factory exist, and they are proven on a fixture and on the real Realtime and Supavisor sources.

Files:
- `bin/nix/firestream/beam/mkMixRelease.nix`, exposed as `firestream.mkMixRelease`. Arguments: `pname`, `version`, `src`, `erlang` (default `erlang_28`), `elixir` (default `elixir_1_19`), `mixDepsHash` (null for no deps), `mixReleaseName`, `mixEnv`, `nif = { cargoRoot; }`, `assets = { npmDepsHash; npmRoot ? "assets"; }`, plus any `mixRelease` argument. It uses `beam_minimal` (no wx, no systemd). It strips the esbuild and tailwind references from `sys.config` and guards them with `disallowedReferences`. It sets `removeCookie = true`. `passthru.beamPackages` holds the set.
- `bin/nix/firestream/containers/beam.nix`, exposed as `firestream.mkBeamContainerModule`. Arguments: `name`, `version`, `release`, `releaseName ? name`, `preStart` (shell run every boot before `start`, with `RELEASE_BIN` set), and the usual `mkContainerModule` arguments. It sets `RELEASE_TMP=<paths.base>/tmp` (an ephemeral runtime dir), `LANG=C.UTF-8` and `ELIXIR_ERL_OPTIONS=+fnu`. If `RELEASE_COOKIE` is not set, it generates one at boot.
- `bin/nix/firestream/default.nix`: the wiring and the exports.
- `bin/nix/firestream/tests/fixtures/beam-hello/`: the fixture release.
- `nix/flake-modules/checks.nix`: `firestream-beam-container`.

Verification:
- `checks.x86_64-linux.firestream-beam-container`: PASS. The image builds, the release closure has 18 paths with no systemd, no COOKIE file ships, `eval` of the migrate function runs, and `start` answers `ok` on TCP.
- `docker run --read-only` with tmpfs on `RELEASE_TMP` and `/tmp`: the pre-start hook printed "migrate ran", and port 4000 answered `ok`.
- Realtime 2.143.3 (OTP 28, Elixir 1.19, assets hook) and Supavisor 2.9.13 (OTP 27, Elixir 1.18, NIF hook) build through `mkMixRelease` with the spike hashes. The `beam_minimal` change did not change the Mix deps hashes. Closures: Realtime 107.6 MiB (spike: 279), Supavisor 111.2 MiB (spike: 228). Both have 0 systemd paths, 0 esbuild/tailwind references and no cookie.
- The Supavisor NIF works: `Supavisor.PgParser.statement_types/1` returns `{:ok, ["SelectStmt", "InsertStmt"]}`.
- Existing images: `containers/base.nix`, `node.nix`, `java.nix` and `apps/base.nix` are unchanged. `default.nix` only adds attributes.
- aarch64: not built (no builder on this host).

For [[FIRE-026]] and [[FIRE-027]]:
- The container `name` must be a valid shell identifier. The base factory builds env names like `<NAME>_BASE_DIR` from it, so `beam-hello` failed at boot. Use `realtime` and `supavisor`, and set `imageName` for a `firestream-supabase-*` image name.
- The working `mkMixRelease` calls, with hashes, are in `~/.cache/firestream-supabase-spikes/FIRE-010/fire017-real.nix`.
- Supavisor's `runtime.exs` needs `SECRET_KEY_BASE`, `VAULT_ENC_KEY`, `API_JWT_SECRET`, `METRICS_JWT_SECRET`, `DATABASE_URL`, `REGION` and `CLUSTER_POSTGRES` even for `eval`.
- A read-only root needs a writable mount at `RELEASE_TMP` and `/tmp`.
