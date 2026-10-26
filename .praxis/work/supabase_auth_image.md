---
id: FIRE-019
title: "Supabase Auth (GoTrue) image"
type: feature
status: done
priority: high
tags: [container, phase-2, supabase]
affects: []
depends-on: [FIRE-018]
---

# Supabase Auth (GoTrue) image

Epic: [[FIRE-009]]

## Goal

Build `supabase/auth` v2.197.0 with Nix and package it as a firestream
image.

## Scope

- Build with `buildGoModule` at a pinned rev. Follow
  `bin/nix/firestream/packages/seaweedfs-weed.nix`. Do not use the nixpkgs
  `gotrue` package: it is the Netlify fork. `gotrue-supabase` in nixpkgs is
  2.188.1, so a pinned build is necessary for 2.197.0.
- Add `src/containers/firestream/supabase-auth/{options.nix,module.nix}`
  and `nix/flake-modules/containers/supabase-auth.nix`. Add the import to
  `flake.nix`.
- Type the main env: `GOTRUE_DB_DATABASE_URL`, `GOTRUE_JWT_SECRET`,
  `API_EXTERNAL_URL` (with `/auth/v1`), `GOTRUE_SITE_URL`, and the SMTP
  settings. Take secrets from [[FIRE-018]].
- Add the image to `containerPackages` in `bin/nix/firestream/ci/profile.nix`.

## Acceptance

- The image builds and `GET /health` answers against the Supabase Postgres
  image.
- The image has no Bitnami binaries.

## Work log

### 2026-10-22

Done. Supabase Auth v2.197.0 builds from source with Nix.

Files:
- `bin/nix/firestream/packages/supabase-auth.nix`: `buildGoModule` at rev `4eee58f2` (tag v2.197.0), `CGO_ENABLED=0`, version ldflag as upstream `make build`. The migrations ship in `share/auth/migrations`, and `bin/gotrue` links to `bin/auth`. It uses `firestream.packages.goToolchain` (Go 1.27.3, from [[FIRE-022]]), because `go.mod` requires go >= 1.27 and the pin has go 1.25.5.
- `src/containers/firestream/supabase-auth/{options.nix,module.nix}`: container `gotrue`, image `firestream-supabase-auth:2.197.0`.
- `nix/flake-modules/containers/supabase-auth.nix`: `packages.supabase-auth`, `firestreamContainers.supabase-auth`, `firestreamImages.supabase-auth`. Also the `flake.nix` import and the `containerPackages` entry.

Env: the upstream compose values. Typed: `GOTRUE_API_HOST/PORT`, `API_EXTERNAL_URL` (default `http://localhost:8000/auth/v1`), `GOTRUE_DB_DRIVER`, `GOTRUE_DB_DATABASE_URL`, `GOTRUE_SITE_URL`, `GOTRUE_URI_ALLOW_LIST`, `GOTRUE_DISABLE_SIGNUP`, `GOTRUE_JWT_*`, the email, phone and anonymous switches, `GOTRUE_SMTP_*` and `GOTRUE_MAILER_URLPATHS_*`. `GOTRUE_JWT_ISSUER` is empty by default, and the entrypoint sets it to `API_EXTERNAL_URL` (the upstream value). `GOTRUE_DB_MIGRATIONS_PATH` points at the store path.

Verification (docker, `--read-only`, against supabase-postgres):
- `GET /health`: 200, `{"version":"v2.197.0","name":"GoTrue",...}`. healthd `/readyz`: 200.
- `POST /signup` (with `GOTRUE_MAILER_AUTOCONFIRM=true`): a user with role `authenticated`. `POST /token?grant_type=password`: a JWT with `iss=http://localhost:8000/auth/v1`, `role=authenticated`.
- The migrations ran at boot as `supabase_auth_admin`. The log has no error lines.
- No Bitnami binaries. `/opt/bitnami` holds only the shared `scripts -> /opt/firestream/scripts` link from the base factory, which every Firestream image has.

Deviations:
- `GOTRUE_SMTP_HOST` is empty and `GOTRUE_SMTP_PORT` is 587 by default. Upstream uses a fake mail host (`supabase-mail:2500`). Compose and the chart must set SMTP or set `GOTRUE_MAILER_AUTOCONFIRM=true`, otherwise an email signup fails.
- `GOTRUE_JWT_KEYS` is not a default key. GoTrue fails on an empty number or boolean, and the env layer exports every key. A consumer adds `GOTRUE_JWT_KEYS` (the `JWT_KEYS` secret) for asymmetric keys.

For Phase 3: port 9999, probe `GET /health`. Needs the database (role `supabase_auth_admin`, password `POSTGRES_PASSWORD`) and `JWT_SECRET`.
