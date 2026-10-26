---
id: FIRE-057
title: "Realtime accepts the gateway's ES256 tokens (JWKS)"
type: feature
status: done
priority: high
tags: [supabase, realtime]
affects: []
depends-on: []
---

# Realtime accepts the gateway's ES256 tokens (JWKS)

Epic: [[FIRE-009]]. Found in [[FIRE-044]].

## Problem

A client that connects to the Realtime websocket with an `sb_publishable_*` key fails. The gateway translates the opaque key into an ES256 JWT (`ANON_KEY_ASYMMETRIC`). Realtime has no JWKS, so it rejects that token with `error_generating_signer`. The legacy anon key (HS256) works.

## Scope

- Give Realtime the JWKS. Upstream v0.8.3 uses `API_JWT_JWKS: ${JWT_JWKS}`. Set it from `firestreamLib.supabase.devSecrets.JWT_JWKS` in:
  - the compose stack (`src/containers/firestream/supabase-envoy/compose.nix`);
  - the chart (`src/charts/firestream/supabase/`), from the key Secret.
- Check that Storage (`JWT_JWKS`) and PostgREST (`PGRST_JWT_SECRET` = JWKS) also accept the asymmetric tokens, in both targets.
- Add an e2e probe: a Realtime websocket joins with the `sb_publishable_*` key and receives an insert event.

## Acceptance

- In compose and on k3d, a websocket with the `sb_publishable_*` key joins and receives a `postgres_changes` event.
- `make test-e2e-supabase` and `make test-e2e-k8s-supabase` pass with the new probe.

## Work log

### 2026-10-23

Start. I read the scope and the FIRE-044 log. Next: compare the service env with upstream v0.8.3.

### 2026-10-23

Env done, and the probe compiles.
- Compose: Auth `GOTRUE_JWT_KEYS`, Realtime `API_JWT_JWKS`, Storage `JWT_JWKS`. PostgREST already had the JWKS.
- Chart: the same three variables come from the key Secret. `JWT_KEYS` is now a required key of the rendered Secret.
- Realtime reads `API_JWT_JWKS` only in the seed. The seed recreates the tenant on each boot, so an old volume gets the JWKS too.
- New probe `supabase_realtime_publishable` in `firestream-e2e-core/src/supabase.rs`. It shares the websocket script with `supabase_realtime_change`.
Next: the checks and the two e2e runs.

### 2026-10-23

Checks and the docker e2e done.
- `checks.supabase-render-fidelity` failed before my change: the chart's values.yaml still had Envoy tag 1.36.2 after FIRE-044. I set it to 1.39.1, and the check passes.
- `checks.firestream-supabase-dev-secrets` passes. `nix flake check --no-build --all-systems` passes. The scan check has nothing to rebuild.
- `make test-e2e-supabase` (STRICT=1): 9 passed, with all probes and `supabase_realtime_publishable`.
- The host snapshot after the run is the same as before. A transient k3d cluster of FIRE-039 showed for a short time and then went away.
Next: `make test-e2e-k8s-supabase`.

### 2026-10-23

Done.

Verification:
- `make test-e2e-k8s-supabase` (K8S_STRICT=1): 14 passed. All probes passed, including `supabase_realtime_publishable`.
- The host snapshot (`docker ps -a`, networks, volumes, `k3d cluster list`) is the same before and after each e2e run.
- In both targets, a websocket with the `sb_publishable_*` key joins and receives the `postgres_changes` insert event.

Files:
- `src/containers/firestream/supabase-envoy/compose.nix`: Auth `GOTRUE_JWT_KEYS`, Realtime `API_JWT_JWKS`, Storage `JWT_JWKS`.
- `src/charts/firestream/supabase/templates/{auth,realtime,storage}/deployment.yaml`: the same variables, from the key Secret.
- `src/charts/firestream/supabase/templates/secret/supabase.yaml`: `JWT_KEYS` is a required key.
- `src/charts/firestream/supabase/values.yaml`: Envoy tag 1.39.1.
- `src/lib/rust/firestream-e2e-core/src/supabase.rs`: the probe `supabase_realtime_publishable`.

Deviations:
- Auth now gets `GOTRUE_JWT_KEYS`, so Auth signs user tokens with ES256. Upstream v0.8.3 shows this as the asymmetric setup.
- A key Secret from `secretRef` must now hold `JWT_KEYS`. `supabase-keygen` always writes it.
- The image options do not change. An empty `API_JWT_JWKS` default would crash the Realtime seed.
- I fixed the stale Envoy tag in values.yaml. It came from FIRE-044 and made the render-fidelity check fail.
- I did not run the probe against a stack without the fix. FIRE-044 recorded that failure.
