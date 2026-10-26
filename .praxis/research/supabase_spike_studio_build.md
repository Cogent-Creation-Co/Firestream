---
id: FIRE-011
title: "Spike: Nix build of Supabase Studio from the monorepo"
type: research
status: done
priority: high
tags: [phase-0, spike, supabase]
affects: []
depends-on: []
---

# Spike: Nix build of Supabase Studio from the monorepo

Epic: [[FIRE-009]]

## Goal

Find out if Nix can build Supabase Studio (`apps/studio` in the
`supabase/supabase` pnpm monorepo) as a Next.js standalone app.

## Questions

- Can a pnpm filter (`--filter studio...`) build only Studio and its
  workspace dependencies?
- Can `firestream.mkNodePackage` (`bin/nix/firestream/node/mkNodePackage.nix`)
  take a monorepo with `pnpmDepsHash`? If not, what must change?
- What are the size of the dependency fetch and the build time?
- Does `output: "standalone"` work for Studio, the same as the nextjs
  container?
- Which env does Studio need in self-host mode (`STUDIO_PG_META_URL`,
  `SUPABASE_URL`, `PG_META_CRYPTO_KEY` and others)?
- Does the build need network access or telemetry that we must turn off?

## Scope

- A throwaway Nix expression that builds Studio.
- No container, no chart.

## Exit

Write the result in the ticket log: "go", "go with changes" or "no-go and
why". FIRE-025 uses this result.

## Result

**Verdict: go with changes.** Nix builds Studio from `supabase/supabase@self-hosted/v0.8.3` as a Next.js standalone app. The build is offline and the app serves pages. But the monorepo pins **pnpm 11.13.1**, and nixpkgs `release-25.11` has only pnpm 10.26.1. `firestream.mkNodePackage` fails unchanged.

Scratch: `/tmp/firestream-supabase-spikes/FIRE-011/studio.nix` (attributes `pnpm11`, `pnpmDeps`, `studio`). Negative test: `mknode.nix`.

### Versions

- Node: `>=22.13` (engines, `engineStrict: true`). nixpkgs `nodejs_22` 22.21.1 works. The upstream image uses `node:22-slim`.
- pnpm: `packageManager: pnpm@11.13.1`. nixpkgs has `pnpm_10` 10.26.1 only. Lockfile is `lockfileVersion: '9.0'`.
- Next.js 16.3.6. `next.config.ts` already sets `output: 'standalone'`.

### Answers

- pnpm filter: yes. `pnpmWorkspaces = [ "studio..." ]` (fetch and install) and `pnpm --filter studio exec next build` build only Studio and its workspace deps. turbo prune is not necessary.
- `mkNodePackage` unchanged: no. With pnpm 10 the deps fetch fails with `ERR_PNPM_UNSUPPORTED_ENGINE`.
- Standalone: yes. Install copies `.next/standalone`, `.next/static` and `public`, the same as the upstream Dockerfile. Start: `node apps/studio/server.js`.
- Network at build: none after two fixes (below). Set `NEXT_TELEMETRY_DISABLED=1`.
- Run test: `/api/platform/profile` 200, `/project/default` 200, `/api/platform/projects` 200. `/api/platform/pg-meta/...` returns 500 until postgres-meta runs. That is expected.

### Working build method

1. pnpm 11: call nixpkgs `pkgs/development/tools/pnpm/generic.nix` with `version = "11.13.1"` and the npm SRI hash `sha512-svx2g7im…RfJQ==`. Override `installPhase`. pnpm 11 ships `bin/pnpm.mjs`, not `bin/pnpm.cjs`. Clear `postInstall` (completions).
2. `fetchPnpmDeps` with `pnpm = pnpm11` and `fetcherVersion = 3`. Remove the line `pnpm config set manage-package-manager-versions false` from its `installPhase` and from `pnpmConfigHook`. pnpm 11 rejects that key with `ERR_PNPM_CONFIG_SET_UNSUPPORTED_YAML_CONFIG_KEY`. The line is not necessary, because pnpm equals the pin.
3. In `postPatch`, set `minimumReleaseAge: 0` in `pnpm-workspace.yaml`. Without it, `pnpm install --offline` asks the registry for publish dates of every package and fails (`EAI_AGAIN`).
4. In `postPatch`, replace `next/font/google` Manrope with `localFont` from nixpkgs `manrope` (`Manrope[wght].ttf`). The Google font download fails in the sandbox.
5. Env: `NODE_OPTIONS=--max-old-space-size=8192`, `STUDIO_FRAMEWORK=next`.

### Size and time (32 cores)

- Deps FOD: 1.7 GB, fetch 5:38. Hash `sha256-y7RhSYsL4Qd7jHj5F2Gk8/Z8f81Im/WfIQmcFhQumtY=`.
- Build: 2:04 (`next build` 1:29).
- Output: 645 MB. Closure: 818 MiB. The closure holds `-dev` outputs (openssl, icu4c, sqlite, libuv, zlib). Trim them in FIRE-025 with `disallowedReferences` or `removeReferencesTo`.

### Runtime env (self-host)

`HOSTNAME=0.0.0.0`, `PORT=3000`, `STUDIO_PG_META_URL`, `POSTGRES_HOST`, `POSTGRES_PORT`, `POSTGRES_DB`, `POSTGRES_PASSWORD`, `POSTGRES_USER_READ_WRITE=postgres`, `PG_META_CRYPTO_KEY`, `PGRST_DB_SCHEMAS`, `PGRST_DB_MAX_ROWS`, `PGRST_DB_EXTRA_SEARCH_PATH`, `DEFAULT_ORGANIZATION_NAME`, `DEFAULT_PROJECT_NAME`, `OPENAI_API_KEY` (optional), `SUPABASE_URL`, `SUPABASE_PUBLIC_URL`, `SUPABASE_ANON_KEY`, `SUPABASE_SERVICE_KEY`, `AUTH_JWT_SECRET`, `SUPABASE_PUBLISHABLE_KEY`, `SUPABASE_SECRET_KEY`, `ENABLED_FEATURES_LOGS_ALL=false`, `SNIPPETS_MANAGEMENT_FOLDER`, `EDGE_FUNCTIONS_MANAGEMENT_FOLDER`, `NEXT_TELEMETRY_DISABLED=1`. Healthcheck: `GET /api/platform/profile`.

### Impact on downstream tickets

- FIRE-025: do not use `mkNodePackage` unchanged. Either extend it with a `pnpm` argument (a pnpm 11 derivation, a pnpm-11-safe fetcher and config hook, and `prePnpmInstall`/`postPatch` hooks), or use a dedicated Studio derivation like the scratch one. Add the font patch, the `minimumReleaseAge` patch and the closure trim.
- The postgres-meta image (separate ticket) is needed for a useful Studio test. This spike did not test postgres-meta.

## Work log

### 2026-10-22

Spike done. Verdict: go with changes. Studio builds offline as a Next.js standalone app with a pnpm 11.13.1 derivation. mkNodePackage needs a pnpm 11 path. See the Result section.
