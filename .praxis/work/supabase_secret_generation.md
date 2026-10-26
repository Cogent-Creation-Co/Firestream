---
id: FIRE-018
title: "Supabase secret and key generation (JWT, anon, service, sb_ keys)"
type: feature
status: done
priority: high
tags: [phase-1, secrets, supabase]
affects: []
depends-on: []
---

# Supabase secret and key generation (JWT, anon, service, sb_ keys)

Epic: [[FIRE-009]]

## Goal

Supply the shared Supabase secrets for development and production.

## Scope

- The secrets are: `POSTGRES_PASSWORD`, `JWT_SECRET`, `ANON_KEY`,
  `SERVICE_ROLE_KEY`, `SECRET_KEY_BASE`, `VAULT_ENC_KEY`, `DB_ENC_KEY`,
  `PG_META_CRYPTO_KEY`. The optional new keys are
  `SUPABASE_PUBLISHABLE_KEY`, `SUPABASE_SECRET_KEY`, `JWT_KEYS` and
  `JWT_JWKS`.
- `ANON_KEY` and `SERVICE_ROLE_KEY` are HS256 JWTs signed with `JWT_SECRET`.
- Development: fixed, documented values in Nix, like the SeaweedFS
  `firestream` / `firestream-secret` credentials. Compose and the chart's
  default values use them.
- Production: the chart reads an existing Kubernetes Secret. Give a small
  tool or Job that generates a full set of consistent keys. Follow upstream
  `utils/add-new-auth-keys.sh` for the sb_ keys.

## Acceptance

- The development keys verify against the development `JWT_SECRET`.
- The generator makes a set that Auth, PostgREST and Envoy all accept.
- The development values are clearly marked as unsafe for production.

## Work log

### 2026-10-22

Done. Fixed development values and a production generator now exist.

Files:
- `bin/nix/firestream/supabase/keygen.py`: one program that reproduces upstream `generate-keys.sh` and `add-new-auth-keys.sh` at `self-hosted/v0.8.3`. It generates `POSTGRES_PASSWORD`, `JWT_SECRET`, `ANON_KEY` and `SERVICE_ROLE_KEY` (HS256), `JWT_KEYS` and `JWT_JWKS` (EC P-256 plus the `oct` legacy key), `ANON_KEY_ASYMMETRIC` and `SERVICE_ROLE_KEY_ASYMMETRIC` (ES256), `SUPABASE_PUBLISHABLE_KEY` and `SUPABASE_SECRET_KEY` (opaque, with the upstream checksum), `SECRET_KEY_BASE`, `REALTIME_DB_ENC_KEY`, `VAULT_ENC_KEY`, `PG_META_CRYPTO_KEY` and `DASHBOARD_PASSWORD`. It has the options `--output env|json|k8s-secret`, `--name`, `--jwt-secret`, `--lifetime` and `--verify FILE`.
- `bin/nix/firestream/supabase/dev-secrets.json`: the published development set. The JWT secret is `firestream-supabase-dev-jwt-secret-not-for-production`, and `POSTGRES_PASSWORD` and `DASHBOARD_PASSWORD` are `firestream-supabase-dev`. The tokens are valid until 2126. The upstream example keys expire on 2027-01-09, so they are not usable.
- `bin/nix/firestream/supabase/default.nix`: `devSecrets` (with a `WARNING`) and `keygen`.
- `nix/flake-modules/supabase.nix`, and its import in `flake.nix`: `packages.supabase-keygen`, `apps.supabase-keygen`, `checks.firestream-supabase-dev-secrets`.

How later tickets use the values:
- Nix: `firestreamLib.supabase.devSecrets.<NAME>`, for example `.ANON_KEY`. In a flake module, `firestreamLib` is a `perSystem` argument.
- Production: `nix run .#supabase-keygen -- --output k8s-secret --name supabase-secrets`.
- The env names are the upstream ones (`REALTIME_DB_ENC_KEY`, not `DB_ENC_KEY`). Each image maps them to its own names.

Verification:
- `checks.x86_64-linux.firestream-supabase-dev-secrets`: PASS. `keygen --verify` checks the dev set. PyJWT verifies all four tokens on its own. A freshly generated set also verifies.
- Manual tests: 8 generated sets verify. A tampered `JWT_SECRET` fails with three findings and exit 1. PyJWT loads the private key from `JWT_KEYS`.
- `nix run .#supabase-keygen -- --output k8s-secret --name demo` prints a v1 Secret with 15 keys.
- `packages.aarch64-linux.supabase-keygen` evaluates.

Deviation: I did not run Envoy's sb_ key translation against these keys. That is part of [[FIRE-021]].
