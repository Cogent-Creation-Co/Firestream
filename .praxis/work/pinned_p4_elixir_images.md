---
id: FIRE-046
title: "P4: Elixir images with per-package Mix deps and source-built tailwind"
type: feature
status: done
priority: high
tags: [beam, nix, supabase]
affects: []
depends-on: [FIRE-042, FIRE-043]
---

# P4: Elixir images with per-package Mix deps and source-built tailwind

Epic: [[FIRE-041]]

## Scope

1. **Realtime assets:** replace nixpkgs `tailwindcss_3`, a GitHub release binary, with tailwind v3.4.17. Build it from the `tailwindlabs/tailwindcss` source at its commit, with pnpm2nix (`bin/nix/firestream/node/pnpm2nix/`). The esbuild that the assets use comes from nixpkgs source. Keep it.
2. **Mix deps:** replace the monolithic `fetchMixDeps` FODs (Realtime and Supavisor, in `bin/nix/firestream/beam/mkMixRelease.nix`) with fetches per package.
   - Generate them from `mix.lock`, in the nixpkgs `mixNixDeps` / mix2nix style, and commit the result as `mix-deps.nix` per app.
   - Hex packages use their lockfile hashes.
   - Git deps (for example `gen_rpc`) use `fetchFromGitHub` at the SHA from the lock.
   - Add a check that regenerates each file and compares it.
3. **`mkMixRelease`** keeps its interface where it can. The BEAM fixture check (`firestream-beam-container`) stays green.

## Acceptance

- No `fetchMixDeps` FOD remains for Realtime or Supavisor.
- The generator checks pass, and a stale file fails them.
- Realtime and Supavisor build. Their Docker acceptance passes: a Realtime websocket change event, and psql through Supavisor on 5432 and 6543.
- The P0 scan passes for both images, and `nix flake check --no-build --all-systems` passes.

## Work log

### 2026-10-23

Milestone: tailwind and the mix-deps generator.

- Tailwind v3.4.17 builds from tailwindlabs/tailwindcss at commit 4f9f603e (tag v3.4.17) with pnpm2nix. File: `src/containers/firestream/supabase-realtime/tailwindcss.nix`.
- The lockfile is a `pnpm import` of the upstream package-lock.json. It adds the four plugins of the standalone CLI (forms, typography, aspect-ratio, container-queries) at the standalone-cli lock versions. The Realtime config requires forms and typography.
- Nixpkgs esbuild replaces @swc/core and the npm esbuild binary. The platform filter drops every npm binary package, so the build fetches none.
- Proof: the CLI compiles the Realtime assets/css with `--minify`. The output has the same size as the nixpkgs standalone binary output (46238 bytes). Only the declaration order differs.
- The generator `bin/nix/firestream/beam/mix-deps.sh` (with `mix-deps.exs`) writes `mix-deps.nix` from each mix.lock. The checks `supabase-realtime-mix-deps` and `supabase-supavisor-mix-deps` pass. A changed version fails the check, and a changed Git commit fails it too. I reverted both edits.

### 2026-10-23

Milestone: Realtime Mix deps. `src/containers/firestream/supabase-realtime/mix-deps.nix` has 115 fetches: 111 Hex packages (fetchHex, lock outer checksum) and 4 Git deps (fetchFromGitHub at the lock commit: gen_rpc, heroicons, phoenix, postgres_replication). mkMixRelease builds the deps/ tree from them; the fetchMixDeps FOD is gone. The assets hook uses the source-built tailwind. The release and the image build. The scanner passes for the release (no unallowed binary). The release closure holds no tailwind, esbuild or deps tree.

### 2026-10-23

Milestone: Supavisor Mix deps. `src/containers/firestream/supavisor/mix-deps.nix` has 85 fetches: 82 Hex packages and 3 Git deps (logflare_logger_backend, poolboy, postgrex). The fetchMixDeps FOD is gone. The release (with the rustler NIF) and the image build. The scanner passes: the only binaries are OTP files and libpgparser.so, which the allowlist covers.

### 2026-10-23

Milestone: acceptance (Docker, supabase-postgres image, POSTGRES_PASSWORD=firestream-supabase-dev).
- Realtime: `/healthcheck` 200. A websocket client joins `realtime:p2` with postgres_changes on public.todos and gets `Subscribed to PostgreSQL`. After an insert it gets `postgres_changes INSERT todos {'id': 2, 'task': 'from-realtime-test'}`. `/assets/app.css` (tailwind output) answers 200.
- Supavisor: psql as `postgres.firestream` through port 5432 (session) and 6543 (transaction) returns the todos count.
- No `[error]` lines in either log.
- `checks.firestream-beam-container` passes.

### 2026-10-23

Done.

Files:
- `bin/nix/firestream/beam/mkMixRelease.nix`: the new `mixDeps` argument replaces `mixDepsHash`. An old caller gets a clear error. `assets.tailwind` replaces `pkgs.tailwindcss_3`.
- `bin/nix/firestream/beam/{mix-deps.exs,mix-deps.sh,mix-deps-check.nix}`: the generator, its wrapper, and the check builder.
- `src/containers/firestream/supabase-realtime/{tailwindcss.nix,pnpm-lock.yaml,pnpm-lock.json,mix-deps.nix,module.nix}`.
- `src/containers/firestream/supavisor/{mix-deps.nix,module.nix}`.
- `nix/flake-modules/containers/{supabase-realtime,supabase-supavisor}.nix`: the pnpm2nix factory argument and the checks `supabase-{realtime,supavisor}-mix-deps`.

Verification:
- The two mix-deps checks and `firestream-pnpm-lock-json` pass.
- A stale version fails the mix-deps check. A stale Git commit fails it too.
- `mix-deps.sh` writes the same files again.
- `firestream-beam-container` passes.
- Both images build. The scanner passes for both releases.
- The Docker acceptance passes (see the entry above).
- `nix flake check --no-build --all-systems --keep-going` ran on a copy of the tree. It found one error only: `studio.libpgQueryWasm` in supabase-pinning.nix, which is FIRE-045 work in progress.

Deviations:
- The deps compile together in the release build, not one derivation per package (buildMix). Reason: Elixir deps read the app config at compile time.
- The generator reads every lock entry, the test and dev deps too. It cannot see the `only:` filter in mix.exs.
- The tailwind CSS has the same size as the output of the nixpkgs binary. The declaration order differs: the npm CLI path of upstream deps.js does not sort declarations.

Open items:
- The Realtime assets npm tree still uses one fetchNpmDeps FOD. Its lock has the oxlint and oxfmt npm binaries as devDependencies. The build does not run them, and the release does not ship them.
