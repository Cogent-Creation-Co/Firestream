---
id: FIRE-025
title: "Studio image"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: [FIRE-011]
---

# Studio image

Epic: [[FIRE-009]]

## Goal

Build Supabase Studio (`apps/studio`, upstream tag
`2026.10.12-sha-3699278`) with Nix and package it as a firestream image.

## Scope

- Use the build method from the spike [[FIRE-011]]: `mkNodePackage` with a
  pnpm monorepo filter, `output: "standalone"`, and
  `mkNodeContainerModule`.
- Add `src/containers/firestream/supabase-studio/{options.nix,module.nix}`,
  the flake-module, the `flake.nix` import and the CI profile entry.
- Type the self-host env: `STUDIO_PG_META_URL`, `SUPABASE_URL`,
  `SUPABASE_PUBLIC_URL`, `SUPABASE_ANON_KEY`, `SUPABASE_SERVICE_KEY`,
  `AUTH_JWT_SECRET`, `PG_META_CRYPTO_KEY`, `DEFAULT_ORGANIZATION_NAME`,
  `DEFAULT_PROJECT_NAME`.
- Turn off telemetry and any outbound calls to Supabase hosted services.

## Acceptance

- The image builds and `GET /api/platform/profile` answers.
- Studio shows the tables of the Supabase Postgres image through
  postgres-meta.

## Spike findings

The spike [[FIRE-011]] says "go with changes":

- The monorepo needs pnpm 11.13.1. The pin has only pnpm 10, so `mkNodePackage` fails without a change. Add a pnpm 11 path to `mkNodePackage`, or use a dedicated derivation.
- Remove the one pnpm config line that pnpm 11 rejects.
- Set `minimumReleaseAge: 0`; otherwise the offline install contacts the registry.
- Replace the Google-hosted Manrope font with the nixpkgs copy.
- Trim the closure. It is 818 MiB today and holds `-dev` outputs.

The working expression is `/tmp/firestream-supabase-spikes/FIRE-011/studio.nix`.

## Node build decision

Use pnpm2nix (see the decisions in [[FIRE-009]]) on the monorepo `pnpm-lock.yaml`. This replaces the single fixed-output dependency fetch from the spike [[FIRE-011]]. Check first that pnpm2nix supports the lockfile format of pnpm 11 and the workspace filter for `apps/studio`.

## Work log

### 2026-10-22

Image done: `packages.supabase-studio` builds `firestream-supabase-studio:2026.10.12` (container name `studio`).

Source: supabase/supabase commit 369927892fde (the commit of image tag `2026.10.12-sha-3699278`), sparse fetch of apps/studio, its 13 linked workspace packages and patches/. The upstream pnpm-lock.yaml (lockfile v9) is used as it is.

Build route: pnpm2nix (the patched `pnpm2nix` input) in workspace mode, with the 14 components and pnpm 11.13.1 (a nixpkgs generic.nix derivation; nixpkgs has only pnpm 10). One fixed-output fetch per locked package replaces the single FOD of the spike FIRE-011. Install scripts do not run. `next build` (Next.js 16.3.6, standalone output).

Patches in the build:
- `minimumReleaseAge: 0` in pnpm-workspace.yaml (else pnpm 11 asks the registry for publish dates).
- Manrope from nixpkgs instead of next/font/google.
- The NEXT_PUBLIC_SUPABASE_URL and NEXT_PUBLIC_SUPABASE_ANON_KEY lines go from apps/studio/.env; they pointed docs search at a hosted Supabase project. The built output has no reference to that project.
- NEXT_TELEMETRY_DISABLED=1. NEXT_PUBLIC_IS_PLATFORM stays unset, so PostHog, GTM and ConfigCat do not start.
- A build check fails on any `.node` file in the standalone output. None ships. Build-time native tools (next-swc, lightningcss, tailwind oxide, rolldown) are prebuilt npm binaries and run only in the build sandbox.

Defect found and fixed: Docker and Kubernetes set HOSTNAME to the container name, and Next.js then listens on that address only. The image takes STUDIO_BIND_ADDRESS (default 0.0.0.0), and the entrypoint exports it as HOSTNAME.

Files: src/containers/firestream/supabase-studio/{options.nix,module.nix,package.nix}, nix/flake-modules/containers/supabase-studio.nix, its flake.nix import, `supabase-studio` in containerPackages (the CI manifest lists it).

Verification (docker, `--read-only --tmpfs /tmp --tmpfs /firestream/studio/data`, with supabase-postgres and postgres-meta):
- `GET /api/platform/profile` 200. healthd `/readyz` 200.
- `/project/default` 200, `/project/default/editor` 200.
- `GET /api/platform/pg-meta/default/tables?included_schemas=public` lists `public.fire_node_probe` through postgres-meta.
- `POST /api/platform/pg-meta/default/query` runs SQL.
- `/api/platform/projects` shows "Default Project", ACTIVE_HEALTHY.

Closure: about 810 MiB (app 599 MiB, of which .next/server 359 MiB and .next/static 114 MiB; Node.js 210 MiB). Not trimmed further.

Facts for Phase 3 (FIRE-028, FIRE-029):
- Port 3000. Readiness: GET /api/platform/profile.
- Env: STUDIO_PG_META_URL (default http://meta:8080), POSTGRES_HOST/PORT/DB/PASSWORD, PG_META_CRYPTO_KEY (must equal postgres-meta CRYPTO_KEY), SUPABASE_URL (default http://api-gw:8000), SUPABASE_PUBLIC_URL, SUPABASE_ANON_KEY, SUPABASE_SERVICE_KEY, AUTH_JWT_SECRET, SUPABASE_PUBLISHABLE_KEY, SUPABASE_SECRET_KEY.
- Mount a writable volume at /firestream/studio/data (snippets and edge function files).
- Do not set HOSTNAME; set STUDIO_BIND_ADDRESS if needed.

Open items: the bundle holds links to supabase.com pages and library default hosts (PostHog, ConfigCat); they are not used in self-host mode, but no network capture was done. The libpg-query WASM is the prebuilt npm file.

### 2026-10-22

PM review: the user allows the prebuilt `libpg-query` WASM file, because the lockfile pins it by hash and WASM is not a native module (decision in [[FIRE-009]]). The IFD in the lockfile parse is removed in [[FIRE-032]]. Open: nobody has captured Studio's network traffic to prove that no PostHog or ConfigCat call happens. [[FIRE-030]] covers that.
