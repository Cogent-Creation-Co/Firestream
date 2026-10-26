---
id: FIRE-010
title: "Spike: Nix build of Realtime and Supavisor (Elixir)"
type: research
status: done
priority: high
tags: [phase-0, spike, supabase]
affects: []
depends-on: []
---

# Spike: Nix build of Realtime and Supavisor (Elixir)

Epic: [[FIRE-009]]

## Goal

Find out if Nix can build `supabase/realtime` (v2.143.3) and
`supabase/supavisor` (2.9.13) as Elixir releases. Firestream has no
Elixir/BEAM builder today. This is the largest unknown in the epic.

## Questions

- Can `beamPackages.mixRelease` build each app?
- How do we fix the Mix dependencies? Compare `fetchMixDeps` (fixed-output)
  with `mix2nix`.
- Do the apps use Rust NIFs (for example through `rustler`)? If yes, how do
  we build them in Nix?
- Which Erlang/OTP and Elixir versions do the apps need? Does nixpkgs
  `release-25.11` have them?
- Which runtime env does each release need (`SECRET_KEY_BASE`,
  `DB_ENC_KEY`, `VAULT_ENC_KEY`, the tenant settings)?
- What are the size and the build time of each release?

## Scope

- A throwaway Nix expression for each app. The expression only has to build.
- No container, no chart.

## Exit

Write the result in the ticket log: "go", "go with changes" or "no-go and
why". Record the versions, the dependency-fetch method and the NIF approach.
FIRE-017, FIRE-026 and FIRE-027 use this result.

## Result

**Verdict: go.** Nix builds both apps from source with `beamPackages.mixRelease`. Both releases boot and serve traffic.

### Versions

| Item | Realtime v2.143.3 | Supavisor 2.9.13 |
|---|---|---|
| Upstream OTP / Elixir | OTP 28.5, Elixir 1.19.5 | OTP 27.2 (`.tool-versions` 27.3.4), Elixir 1.18.2 |
| nixpkgs `release-25.11` | `erlang_28` 28.3, `elixir_1_19` 1.19.5 | `erlang_27` 27.3.4.6, `elixir_1_18` 1.18.4 |
| Rust NIF | none | `native/pgparser` (rustler 0.36.1, `pg_query` 6.1.0); rustc 1.91.1 |
| Assets | esbuild 0.14.29, tailwind 3.3.2, npm (`@supabase/realtime-js`) | none |

No nixpkgs bump is necessary.

### Working build method

Scratch expressions: `/tmp/firestream-supabase-spikes/FIRE-010/realtime.nix` and `supavisor.nix`.

- Beam set: `pkgs.beam.packages.erlang_28.extend (s: _: { elixir = s.elixir_1_19; })` for Realtime. Use `erlang_27` + `elixir_1_18` for Supavisor.
- Mix deps: `beamPackages.fetchMixDeps` (fixed-output, one hash per app). Git deps (phoenix fork, gen_rpc, poolboy, postgrex, logflare_logger_backend) work in the FOD. `mix2nix` is not necessary. Use `fetchMixDeps`, because it keeps the lock as the single source.
- Hashes: Realtime deps `sha256-BLp3I32IU7mKyn2QhnzvxpIUO0npsaGhxFidtFRSk/w=`, Realtime npm `sha256-blPzISiJxZ1iPB0KaT6E/Sq9axEYRGGbu0X+4jwhNpI=`, Supavisor deps `sha256-zIt7Vf8R6xjAu9Y4nXtOdlqTE4RLPaotYJ7l13GyveU=`.
- Realtime assets: `fetchNpmDeps` + `npmConfigHook` with `npmRoot = "assets"`. Append `config :esbuild, path: ...` and `config :tailwind, path: ...` to `config/config.exs` in `postPatch`. Point them at nixpkgs `esbuild` 0.25.5 and `tailwindcss_3` 3.4.17. Run `mix assets.deploy` in `postBuild`. Without the path config, esbuild tries a download and the build fails.
- Supavisor NIF: let rustler run cargo inside `mixRelease`. Add `cargoDeps = rustPlatform.importCargoLock { lockFile = "${src}/native/Cargo.lock"; }`, `cargoRoot = "native"`, `cargo`, `rustc`, `rustPlatform.cargoSetupHook`, `rustPlatform.bindgenHook` and `cmake` (with `dontUseCmakeConfigure = true`). No precompiled NIF and no network. `Supavisor.PgParser.statement_types/1` returns `{:ok, ["SelectStmt", "InsertStmt"]}` at runtime.
- Supavisor: set `mixReleaseName = "supavisor"`.

### Size and time (32-core workstation, warm deps)

- Realtime: build 0:42. Release 51 MiB. Closure 279 MiB.
- Supavisor: build 2:15 (most of it is the Rust NIF). Release 53 MiB. Closure 228 MiB.
- 172 MiB of each closure is `systemd`. Only `erts-*/bin/epmd` links it. Fix: override erlang with `systemdSupport = false`, or patch `epmd`.
- Realtime `sys.config` keeps references to esbuild and tailwind. Fix: `removeReferencesTo` or `disallowedReferences`.

### Runtime test (against the FIRE-012 database)

- Realtime: run `bin/migrate`, then `bin/realtime eval 'Realtime.Release.seeds(Realtime.Repo)'` (`SEED_SELF_HOST`), then `bin/server`. `GET /api/tenants/realtime-dev/health` with an anon JWT returns 200 and `"healthy":true`. The upstream `run.sh` does these steps. The container must do them too.
- Supavisor: run `bin/migrate`, then `bin/supavisor eval "$(cat pooler.exs)"`, then `bin/server`. `psql` through the transaction port and the session port as `postgres.<tenant>` returns rows. Use `eval`, not `rpc`. `rpc` fails with "Hostname ... is illegal".

### Required env

- Realtime: `PORT`, `DB_HOST`, `DB_PORT`, `DB_USER=supabase_admin`, `DB_PASSWORD`, `DB_NAME`, `DB_AFTER_CONNECT_QUERY='SET search_path TO _realtime'`, `DB_ENC_KEY` (16 bytes), `API_JWT_SECRET`, `SECRET_KEY_BASE`, `METRICS_JWT_SECRET`, `APP_NAME`, `SEED_SELF_HOST=true`, `RUN_JANITOR=true`, `DNS_NODES="''"`, `ERL_AFLAGS='-proto_dist inet_tcp'`, `RLIMIT_NOFILE`. `runtime.exs` reads `GEN_RPC_CACERTFILE`, `GEN_RPC_CERTFILE` and `GEN_RPC_KEYFILE` only when the gen_rpc SSL port is set. The default TCP driver does not need them. `DB_IP_VERSION=ipv4` stops the IPv6 probe.
- Supavisor: `PORT`, `PROXY_PORT_SESSION`, `PROXY_PORT_TRANSACTION`, `DATABASE_URL` (to `_supabase`), `CLUSTER_POSTGRES=true`, `SECRET_KEY_BASE`, `VAULT_ENC_KEY` (32 chars), `API_JWT_SECRET`, `METRICS_JWT_SECRET`, `REGION`, `ERL_AFLAGS`, `DB_POOL_SIZE`, and the `POOLER_*` values for `pooler.exs`.
- Both: set `RELEASE_COOKIE`. The spike used `removeCookie = false`; production must inject the cookie.

### Problems and workarounds

- `pgdelta` (Realtime dashboard "tenant migrations" page) is a Bun-compiled binary from `supabase/pg-toolbelt`. The spike omits it. Only that admin page needs it. Package it later or accept the gap.
- `rebar3_hex` and `rebar3_ex_doc` plugin fetch errors in the log are harmless.

### Impact on downstream tickets

- FIRE-017 (Elixir builder): use `mixRelease` + `fetchMixDeps`. Support a per-app beam set, `npmRoot` assets, and a cargo NIF seam. Add the epmd/systemd and the reference cleanup.
- FIRE-026 (Realtime): entrypoint must run migrate + seed before `server`. Keep the gen_rpc TCP driver (no certs) for a single node. Decide on `pgdelta`.
- FIRE-027 (Supavisor): entrypoint must run migrate + `eval pooler.exs` before `server`. Keep OTP 27 / Elixir 1.18. Do not move it to OTP 28 without a test.

## Work log

### 2026-10-22

Spike done. Verdict: go. Both releases build with mixRelease and fetchMixDeps, and both boot against the FIRE-012 database. See the Result section.
